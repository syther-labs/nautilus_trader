// -------------------------------------------------------------------------------------------------
//  Copyright (C) 2015-2024 Nautech Systems Pty Ltd. All rights reserved.
//  https://nautechsystems.io
//
//  Licensed under the GNU Lesser General Public License Version 3.0 (the "License");
//  You may not use this file except in compliance with the License.
//  You may obtain a copy of the License at https://www.gnu.org/licenses/lgpl-3.0.en.html
//
//  Unless required by applicable law or agreed to in writing, software
//  distributed under the License is distributed on an "AS IS" BASIS,
//  WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
//  See the License for the specific language governing permissions and
//  limitations under the License.
// -------------------------------------------------------------------------------------------------

use std::{
    cmp,
    ffi::{c_char, CStr},
    i64,
    str::FromStr,
};

use anyhow::{anyhow, bail, Result};
use databento::dbn::Record;
use nautilus_core::{datetime::NANOSECONDS_IN_SECOND, time::UnixNanos};
use nautilus_model::{
    data::{
        bar::{Bar, BarSpecification, BarType},
        delta::OrderBookDelta,
        depth::{OrderBookDepth10, DEPTH10_LEN},
        order::BookOrder,
        quote::QuoteTick,
        trade::TradeTick,
        Data,
    },
    enums::{
        AggregationSource, AggressorSide, AssetClass, BarAggregation, BookAction, InstrumentClass,
        OptionKind, OrderSide, PriceType,
    },
    identifiers::{instrument_id::InstrumentId, trade_id::TradeId},
    instruments::{
        equity::Equity, futures_contract::FuturesContract, futures_spread::FuturesSpread,
        options_contract::OptionsContract, options_spread::OptionsSpread, Instrument,
    },
    types::{currency::Currency, fixed::FIXED_SCALAR, price::Price, quantity::Quantity},
};
use ustr::Ustr;

const BAR_SPEC_1S: BarSpecification = BarSpecification {
    step: 1,
    aggregation: BarAggregation::Second,
    price_type: PriceType::Last,
};
const BAR_SPEC_1M: BarSpecification = BarSpecification {
    step: 1,
    aggregation: BarAggregation::Minute,
    price_type: PriceType::Last,
};
const BAR_SPEC_1H: BarSpecification = BarSpecification {
    step: 1,
    aggregation: BarAggregation::Hour,
    price_type: PriceType::Last,
};
const BAR_SPEC_1D: BarSpecification = BarSpecification {
    step: 1,
    aggregation: BarAggregation::Day,
    price_type: PriceType::Last,
};

const BAR_CLOSE_ADJUSTMENT_1S: u64 = NANOSECONDS_IN_SECOND;
const BAR_CLOSE_ADJUSTMENT_1M: u64 = NANOSECONDS_IN_SECOND * 60;
const BAR_CLOSE_ADJUSTMENT_1H: u64 = NANOSECONDS_IN_SECOND * 60 * 60;
const BAR_CLOSE_ADJUSTMENT_1D: u64 = NANOSECONDS_IN_SECOND * 60 * 60 * 24;

#[must_use]
pub fn parse_order_side(c: c_char) -> OrderSide {
    match c as u8 as char {
        'A' => OrderSide::Sell,
        'B' => OrderSide::Buy,
        _ => OrderSide::NoOrderSide,
    }
}

#[must_use]
pub fn parse_aggressor_side(c: c_char) -> AggressorSide {
    match c as u8 as char {
        'A' => AggressorSide::Seller,
        'B' => AggressorSide::Buyer,
        _ => AggressorSide::NoAggressor,
    }
}

pub fn parse_book_action(c: c_char) -> Result<BookAction> {
    match c as u8 as char {
        'A' => Ok(BookAction::Add),
        'C' => Ok(BookAction::Delete),
        'F' => Ok(BookAction::Update),
        'M' => Ok(BookAction::Update),
        'R' => Ok(BookAction::Clear),
        _ => bail!("Invalid `BookAction`, was '{c}'"),
    }
}

pub fn parse_option_kind(c: c_char) -> Result<OptionKind> {
    match c as u8 as char {
        'C' => Ok(OptionKind::Call),
        'P' => Ok(OptionKind::Put),
        _ => bail!("Invalid `OptionKind`, was '{c}'"),
    }
}

pub fn parse_cfi_iso10926(value: &str) -> Result<(Option<AssetClass>, Option<InstrumentClass>)> {
    let chars: Vec<char> = value.chars().collect();
    if chars.len() < 3 {
        bail!("Value string is too short");
    }

    let cfi_category = chars[0];
    let cfi_group = chars[1];
    let cfi_attribute1 = chars[2];
    // let cfi_attribute2 = value[3];
    // let cfi_attribute3 = value[4];
    // let cfi_attribute4 = value[5];

    let mut asset_class = match cfi_category {
        'D' => Some(AssetClass::Debt),
        'E' => Some(AssetClass::Equity),
        'S' => None,
        _ => None,
    };

    let instrument_class = match cfi_group {
        'I' => Some(InstrumentClass::Future),
        _ => None,
    };

    if cfi_attribute1 == 'I' {
        asset_class = Some(AssetClass::Index);
    }

    Ok((asset_class, instrument_class))
}

pub fn decode_min_price_increment(value: i64, currency: Currency) -> Result<Price> {
    match value {
        0 | i64::MAX => Price::new(
            10f64.powi(-i32::from(currency.precision)),
            currency.precision,
        ),
        _ => Price::from_raw(value, currency.precision),
    }
}

/// # Safety
///
/// - Assumes `ptr` is a valid C string pointer.
pub unsafe fn raw_ptr_to_string(ptr: *const c_char) -> Result<String> {
    let c_str: &CStr = unsafe { CStr::from_ptr(ptr) };
    let str_slice: &str = c_str.to_str().map_err(|e| anyhow!(e))?;
    Ok(str_slice.to_owned())
}

/// # Safety
///
/// - Assumes `ptr` is a valid C string pointer.
pub unsafe fn raw_ptr_to_ustr(ptr: *const c_char) -> Result<Ustr> {
    let c_str: &CStr = unsafe { CStr::from_ptr(ptr) };
    let str_slice: &str = c_str.to_str().map_err(|e| anyhow!(e))?;
    Ok(Ustr::from(str_slice))
}

pub fn decode_equity_v1(
    msg: &dbn::compat::InstrumentDefMsgV1,
    instrument_id: InstrumentId,
    ts_init: UnixNanos,
) -> Result<Equity> {
    let currency = Currency::USD(); // TODO: Temporary hard coding of US equities for now

    Equity::new(
        instrument_id,
        instrument_id.symbol,
        None, // No ISIN available yet
        currency,
        currency.precision,
        decode_min_price_increment(msg.min_price_increment, currency)?,
        Some(Quantity::new(msg.min_lot_size_round_lot.into(), 0)?),
        None,        // TBD
        None,        // TBD
        None,        // TBD
        None,        // TBD
        msg.ts_recv, // More accurate and reliable timestamp
        ts_init,
    )
}

pub fn decode_futures_contract_v1(
    msg: &dbn::compat::InstrumentDefMsgV1,
    instrument_id: InstrumentId,
    ts_init: UnixNanos,
) -> Result<FuturesContract> {
    let currency = Currency::USD(); // TODO: Temporary hard coding of US futures for now
    let cfi_str = unsafe { raw_ptr_to_string(msg.cfi.as_ptr())? };
    let underlying = unsafe { raw_ptr_to_ustr(msg.asset.as_ptr())? };
    let (asset_class, _) = parse_cfi_iso10926(&cfi_str)?;

    FuturesContract::new(
        instrument_id,
        instrument_id.symbol,
        asset_class.unwrap_or(AssetClass::Commodity),
        underlying,
        msg.activation,
        msg.expiration,
        currency,
        currency.precision,
        decode_min_price_increment(msg.min_price_increment, currency)?,
        Quantity::new(1.0, 0)?, // TBD
        Quantity::new(1.0, 0)?, // TBD
        None,                   // TBD
        None,                   // TBD
        None,                   // TBD
        None,                   // TBD
        msg.ts_recv,            // More accurate and reliable timestamp
        ts_init,
    )
}

pub fn decode_futures_spread_v1(
    msg: &dbn::compat::InstrumentDefMsgV1,
    instrument_id: InstrumentId,
    ts_init: UnixNanos,
) -> Result<FuturesSpread> {
    let currency = Currency::USD(); // TODO: Temporary hard coding of US futures for now
    let cfi_str = unsafe { raw_ptr_to_string(msg.cfi.as_ptr())? };
    let underlying = unsafe { raw_ptr_to_ustr(msg.asset.as_ptr())? };
    let strategy_type = unsafe { raw_ptr_to_ustr(msg.secsubtype.as_ptr())? };
    let (asset_class, _) = parse_cfi_iso10926(&cfi_str)?;

    FuturesSpread::new(
        instrument_id,
        instrument_id.symbol,
        asset_class.unwrap_or(AssetClass::Commodity),
        underlying,
        strategy_type,
        msg.activation,
        msg.expiration,
        currency,
        currency.precision,
        decode_min_price_increment(msg.min_price_increment, currency)?,
        Quantity::new(1.0, 0)?, // TBD
        Quantity::new(1.0, 0)?, // TBD
        None,                   // TBD
        None,                   // TBD
        None,                   // TBD
        None,                   // TBD
        msg.ts_recv,            // More accurate and reliable timestamp
        ts_init,
    )
}

pub fn decode_options_contract_v1(
    msg: &dbn::compat::InstrumentDefMsgV1,
    instrument_id: InstrumentId,
    ts_init: UnixNanos,
) -> Result<OptionsContract> {
    let currency_str = unsafe { raw_ptr_to_string(msg.currency.as_ptr())? };
    let cfi_str = unsafe { raw_ptr_to_string(msg.cfi.as_ptr())? };
    let asset_class_opt = match instrument_id.venue.value.as_str() {
        "OPRA" => Some(AssetClass::Equity),
        _ => {
            let (asset_class, _) = parse_cfi_iso10926(&cfi_str)?;
            asset_class
        }
    };
    let underlying = unsafe { raw_ptr_to_ustr(msg.underlying.as_ptr())? };
    let currency = Currency::from_str(&currency_str)?;

    OptionsContract::new(
        instrument_id,
        instrument_id.symbol,
        asset_class_opt.unwrap_or(AssetClass::Commodity),
        underlying,
        parse_option_kind(msg.instrument_class)?,
        msg.activation,
        msg.expiration,
        Price::from_raw(msg.strike_price, currency.precision)?,
        currency,
        currency.precision,
        decode_min_price_increment(msg.min_price_increment, currency)?,
        Quantity::new(1.0, 0)?, // TBD
        Quantity::new(1.0, 0)?, // TBD
        None,                   // TBD
        None,                   // TBD
        None,                   // TBD
        None,                   // TBD
        msg.ts_recv,            // More accurate and reliable timestamp
        ts_init,
    )
}

pub fn decode_options_spread_v1(
    msg: &dbn::compat::InstrumentDefMsgV1,
    instrument_id: InstrumentId,
    ts_init: UnixNanos,
) -> Result<OptionsSpread> {
    let currency_str = unsafe { raw_ptr_to_string(msg.currency.as_ptr())? };
    let cfi_str = unsafe { raw_ptr_to_string(msg.cfi.as_ptr())? };
    let asset_class_opt = match instrument_id.venue.value.as_str() {
        "OPRA" => Some(AssetClass::Equity),
        _ => {
            let (asset_class, _) = parse_cfi_iso10926(&cfi_str)?;
            asset_class
        }
    };
    let underlying = unsafe { raw_ptr_to_ustr(msg.underlying.as_ptr())? };
    let strategy_type = unsafe { raw_ptr_to_ustr(msg.secsubtype.as_ptr())? };
    let currency = Currency::from_str(&currency_str)?;

    OptionsSpread::new(
        instrument_id,
        instrument_id.symbol,
        asset_class_opt.unwrap_or(AssetClass::Commodity),
        underlying,
        strategy_type,
        msg.activation,
        msg.expiration,
        currency,
        currency.precision,
        decode_min_price_increment(msg.min_price_increment, currency)?,
        Quantity::new(1.0, 0)?, // TBD
        Quantity::new(1.0, 0)?, // TBD
        None,                   // TBD
        None,                   // TBD
        None,                   // TBD
        None,                   // TBD
        msg.ts_recv,            // More accurate and reliable timestamp
        ts_init,
    )
}

#[must_use]
pub fn is_trade_msg(order_side: OrderSide, action: c_char) -> bool {
    order_side == OrderSide::NoOrderSide || action as u8 as char == 'T'
}

pub fn decode_mbo_msg(
    msg: &dbn::MboMsg,
    instrument_id: InstrumentId,
    price_precision: u8,
    ts_init: UnixNanos,
    include_trades: bool,
) -> Result<(Option<OrderBookDelta>, Option<TradeTick>)> {
    let side = parse_order_side(msg.side);
    if is_trade_msg(side, msg.action) {
        if include_trades {
            let trade = TradeTick::new(
                instrument_id,
                Price::from_raw(msg.price, price_precision)?,
                Quantity::from_raw(u64::from(msg.size) * FIXED_SCALAR as u64, 0)?,
                parse_aggressor_side(msg.side),
                TradeId::new(itoa::Buffer::new().format(msg.sequence))?,
                msg.ts_recv,
                ts_init,
            );
            return Ok((None, Some(trade)));
        } else {
            return Ok((None, None));
        }
    };

    let order = BookOrder::new(
        side,
        Price::from_raw(msg.price, price_precision)?,
        Quantity::from_raw(u64::from(msg.size) * FIXED_SCALAR as u64, 0)?,
        msg.order_id,
    );

    let delta = OrderBookDelta::new(
        instrument_id,
        parse_book_action(msg.action)?,
        order,
        msg.flags,
        msg.sequence.into(),
        msg.ts_recv,
        ts_init,
    );

    Ok((Some(delta), None))
}

pub fn decode_trade_msg(
    msg: &dbn::TradeMsg,
    instrument_id: InstrumentId,
    price_precision: u8,
    ts_init: UnixNanos,
) -> Result<TradeTick> {
    let trade = TradeTick::new(
        instrument_id,
        Price::from_raw(msg.price, price_precision)?,
        Quantity::from_raw(u64::from(msg.size) * FIXED_SCALAR as u64, 0)?,
        parse_aggressor_side(msg.side),
        TradeId::new(itoa::Buffer::new().format(msg.sequence))?,
        msg.ts_recv,
        ts_init,
    );

    Ok(trade)
}

pub fn decode_mbp1_msg(
    msg: &dbn::Mbp1Msg,
    instrument_id: InstrumentId,
    price_precision: u8,
    ts_init: UnixNanos,
    include_trades: bool,
) -> Result<(QuoteTick, Option<TradeTick>)> {
    let top_level = &msg.levels[0];
    let quote = QuoteTick::new(
        instrument_id,
        Price::from_raw(top_level.bid_px, price_precision)?,
        Price::from_raw(top_level.ask_px, price_precision)?,
        Quantity::from_raw(u64::from(top_level.bid_sz) * FIXED_SCALAR as u64, 0)?,
        Quantity::from_raw(u64::from(top_level.ask_sz) * FIXED_SCALAR as u64, 0)?,
        msg.ts_recv,
        ts_init,
    )?;

    let maybe_trade = if include_trades && msg.action as u8 as char == 'T' {
        Some(TradeTick::new(
            instrument_id,
            Price::from_raw(msg.price, price_precision)?,
            Quantity::from_raw(u64::from(msg.size) * FIXED_SCALAR as u64, 0)?,
            parse_aggressor_side(msg.side),
            TradeId::new(itoa::Buffer::new().format(msg.sequence))?,
            msg.ts_recv,
            ts_init,
        ))
    } else {
        None
    };

    Ok((quote, maybe_trade))
}

pub fn decode_mbp10_msg(
    msg: &dbn::Mbp10Msg,
    instrument_id: InstrumentId,
    price_precision: u8,
    ts_init: UnixNanos,
) -> Result<OrderBookDepth10> {
    let mut bids = Vec::with_capacity(DEPTH10_LEN);
    let mut asks = Vec::with_capacity(DEPTH10_LEN);
    let mut bid_counts = Vec::with_capacity(DEPTH10_LEN);
    let mut ask_counts = Vec::with_capacity(DEPTH10_LEN);

    for level in &msg.levels {
        let bid_order = BookOrder::new(
            OrderSide::Buy,
            Price::from_raw(level.bid_px, price_precision)?,
            Quantity::from_raw(u64::from(level.bid_sz) * FIXED_SCALAR as u64, 0)?,
            0,
        );

        let ask_order = BookOrder::new(
            OrderSide::Sell,
            Price::from_raw(level.ask_px, price_precision)?,
            Quantity::from_raw(u64::from(level.ask_sz) * FIXED_SCALAR as u64, 0)?,
            0,
        );

        bids.push(bid_order);
        asks.push(ask_order);
        bid_counts.push(level.bid_ct);
        ask_counts.push(level.ask_ct);
    }

    let bids: [BookOrder; DEPTH10_LEN] = bids.try_into().expect("`bids` length != 10");
    let asks: [BookOrder; DEPTH10_LEN] = asks.try_into().expect("`asks` length != 10");
    let bid_counts: [u32; DEPTH10_LEN] = bid_counts.try_into().expect("`bid_counts` length != 10");
    let ask_counts: [u32; DEPTH10_LEN] = ask_counts.try_into().expect("`ask_counts` length != 10");

    let depth = OrderBookDepth10::new(
        instrument_id,
        bids,
        asks,
        bid_counts,
        ask_counts,
        msg.flags,
        msg.sequence.into(),
        msg.ts_recv,
        ts_init,
    );

    Ok(depth)
}

pub fn decode_bar_type(msg: &dbn::OhlcvMsg, instrument_id: InstrumentId) -> Result<BarType> {
    let bar_type = match msg.hd.rtype {
        32 => {
            // ohlcv-1s
            BarType::new(instrument_id, BAR_SPEC_1S, AggregationSource::External)
        }
        33 => {
            //  ohlcv-1m
            BarType::new(instrument_id, BAR_SPEC_1M, AggregationSource::External)
        }
        34 => {
            // ohlcv-1h
            BarType::new(instrument_id, BAR_SPEC_1H, AggregationSource::External)
        }
        35 => {
            // ohlcv-1d
            BarType::new(instrument_id, BAR_SPEC_1D, AggregationSource::External)
        }
        _ => bail!(
            "`rtype` is not a supported bar aggregation, was {}",
            msg.hd.rtype
        ),
    };

    Ok(bar_type)
}

pub fn decode_ts_event_adjustment(msg: &dbn::OhlcvMsg) -> Result<UnixNanos> {
    let adjustment = match msg.hd.rtype {
        32 => {
            // ohlcv-1s
            BAR_CLOSE_ADJUSTMENT_1S
        }
        33 => {
            //  ohlcv-1m
            BAR_CLOSE_ADJUSTMENT_1M
        }
        34 => {
            //  ohlcv-1h
            BAR_CLOSE_ADJUSTMENT_1H
        }
        35 => {
            // ohlcv-1d
            BAR_CLOSE_ADJUSTMENT_1D
        }
        _ => bail!(
            "`rtype` is not a supported bar aggregation, was {}",
            msg.hd.rtype
        ),
    };

    Ok(adjustment)
}

pub fn decode_ohlcv_msg(
    msg: &dbn::OhlcvMsg,
    instrument_id: InstrumentId,
    price_precision: u8,
    ts_init: UnixNanos,
) -> Result<Bar> {
    let bar_type = decode_bar_type(msg, instrument_id)?;
    let ts_event_adjustment = decode_ts_event_adjustment(msg)?;

    // Adjust `ts_event` from open to close of bar
    let ts_event = msg.hd.ts_event;
    let ts_init = cmp::max(ts_init, ts_event) + ts_event_adjustment;

    let bar = Bar::new(
        bar_type,
        Price::from_raw(msg.open / 100, price_precision)?, // TODO(adjust for display factor)
        Price::from_raw(msg.high / 100, price_precision)?, // TODO(adjust for display factor)
        Price::from_raw(msg.low / 100, price_precision)?,  // TODO(adjust for display factor)
        Price::from_raw(msg.close / 100, price_precision)?, // TODO(adjust for display factor)
        Quantity::from_raw(msg.volume * FIXED_SCALAR as u64, 0)?, // TODO(adjust for display factor)
        ts_event,
        ts_init,
    );

    Ok(bar)
}

pub fn decode_record(
    rec_ref: &dbn::RecordRef,
    instrument_id: InstrumentId,
    price_precision: u8,
    ts_init: Option<UnixNanos>,
    include_trades: bool,
) -> Result<(Option<Data>, Option<Data>)> {
    let rtype = rec_ref.rtype().expect("Invalid `rtype`");
    let result = match rtype {
        dbn::RType::Mbo => {
            let msg = rec_ref.get::<dbn::MboMsg>().unwrap(); // SAFETY: RType known
            let ts_init = match ts_init {
                Some(ts_init) => ts_init,
                None => msg.ts_recv,
            };
            let result =
                decode_mbo_msg(msg, instrument_id, price_precision, ts_init, include_trades)?;
            match result {
                (Some(delta), None) => (Some(Data::Delta(delta)), None),
                (None, Some(trade)) => (Some(Data::Trade(trade)), None),
                (None, None) => (None, None),
                _ => bail!("Invalid `MboMsg` parsing combination"),
            }
        }
        dbn::RType::Mbp0 => {
            let msg = rec_ref.get::<dbn::TradeMsg>().unwrap(); // SAFETY: RType known
            let ts_init = match ts_init {
                Some(ts_init) => ts_init,
                None => msg.ts_recv,
            };
            let trade = decode_trade_msg(msg, instrument_id, price_precision, ts_init)?;
            (Some(Data::Trade(trade)), None)
        }
        dbn::RType::Mbp1 => {
            let msg = rec_ref.get::<dbn::Mbp1Msg>().unwrap(); // SAFETY: RType known
            let ts_init = match ts_init {
                Some(ts_init) => ts_init,
                None => msg.ts_recv,
            };
            let result =
                decode_mbp1_msg(msg, instrument_id, price_precision, ts_init, include_trades)?;
            match result {
                (quote, None) => (Some(Data::Quote(quote)), None),
                (quote, Some(trade)) => (Some(Data::Quote(quote)), Some(Data::Trade(trade))),
            }
        }
        dbn::RType::Mbp10 => {
            let msg = rec_ref.get::<dbn::Mbp10Msg>().unwrap(); // SAFETY: RType known
            let ts_init = match ts_init {
                Some(ts_init) => ts_init,
                None => msg.ts_recv,
            };
            let depth = decode_mbp10_msg(msg, instrument_id, price_precision, ts_init)?;
            (Some(Data::Depth10(depth)), None)
        }
        dbn::RType::Ohlcv1S
        | dbn::RType::Ohlcv1M
        | dbn::RType::Ohlcv1H
        | dbn::RType::Ohlcv1D
        | dbn::RType::OhlcvEod => {
            let msg = rec_ref.get::<dbn::OhlcvMsg>().unwrap(); // SAFETY: RType known
            let ts_init = match ts_init {
                Some(ts_init) => ts_init,
                None => msg.hd.ts_event,
            };
            let bar = decode_ohlcv_msg(msg, instrument_id, price_precision, ts_init)?;
            (Some(Data::Bar(bar)), None)
        }
        _ => bail!("RType {:?} is not currently supported", rtype),
    };

    Ok(result)
}

pub fn decode_instrument_def_msg_v1(
    msg: &dbn::compat::InstrumentDefMsgV1,
    instrument_id: InstrumentId,
    ts_init: UnixNanos,
) -> Result<Box<dyn Instrument>> {
    match msg.instrument_class as u8 as char {
        'K' => Ok(Box::new(decode_equity_v1(msg, instrument_id, ts_init)?)),
        'F' => Ok(Box::new(decode_futures_contract_v1(
            msg,
            instrument_id,
            ts_init,
        )?)),
        'S' => Ok(Box::new(decode_futures_spread_v1(
            msg,
            instrument_id,
            ts_init,
        )?)),
        'C' | 'P' => Ok(Box::new(decode_options_contract_v1(
            msg,
            instrument_id,
            ts_init,
        )?)),
        'T' | 'M' => Ok(Box::new(decode_options_spread_v1(
            msg,
            instrument_id,
            ts_init,
        )?)),
        'B' => bail!("Unsupported `instrument_class` 'B' (BOND)"),
        'X' => bail!("Unsupported `instrument_class` 'X' (FX_SPOT)"),
        _ => bail!(
            "Unsupported `instrument_class` '{}'",
            msg.instrument_class as u8 as char
        ),
    }
}

pub fn decode_instrument_def_msg(
    msg: &dbn::InstrumentDefMsg,
    instrument_id: InstrumentId,
    ts_init: UnixNanos,
) -> Result<Box<dyn Instrument>> {
    match msg.instrument_class as u8 as char {
        'K' => Ok(Box::new(decode_equity(msg, instrument_id, ts_init)?)),
        'F' => Ok(Box::new(decode_futures_contract(
            msg,
            instrument_id,
            ts_init,
        )?)),
        'S' => Ok(Box::new(decode_futures_spread(
            msg,
            instrument_id,
            ts_init,
        )?)),
        'C' | 'P' => Ok(Box::new(decode_options_contract(
            msg,
            instrument_id,
            ts_init,
        )?)),
        'T' | 'M' => Ok(Box::new(decode_options_spread(
            msg,
            instrument_id,
            ts_init,
        )?)),
        'B' => bail!("Unsupported `instrument_class` 'B' (BOND)"),
        'X' => bail!("Unsupported `instrument_class` 'X' (FX_SPOT)"),
        _ => bail!(
            "Unsupported `instrument_class` '{}'",
            msg.instrument_class as u8 as char
        ),
    }
}

pub fn decode_equity(
    msg: &dbn::InstrumentDefMsg,
    instrument_id: InstrumentId,
    ts_init: UnixNanos,
) -> Result<Equity> {
    let currency = Currency::USD(); // TODO: Temporary hard coding of US equities for now

    Equity::new(
        instrument_id,
        instrument_id.symbol,
        None, // No ISIN available yet
        currency,
        currency.precision,
        decode_min_price_increment(msg.min_price_increment, currency)?,
        Some(Quantity::new(msg.min_lot_size_round_lot.into(), 0)?),
        None,        // TBD
        None,        // TBD
        None,        // TBD
        None,        // TBD
        msg.ts_recv, // More accurate and reliable timestamp
        ts_init,
    )
}

pub fn decode_futures_contract(
    msg: &dbn::InstrumentDefMsg,
    instrument_id: InstrumentId,
    ts_init: UnixNanos,
) -> Result<FuturesContract> {
    let currency = Currency::USD(); // TODO: Temporary hard coding of US futures for now
    let cfi_str = unsafe { raw_ptr_to_string(msg.cfi.as_ptr())? };
    let underlying = unsafe { raw_ptr_to_ustr(msg.asset.as_ptr())? };
    let (asset_class, _) = parse_cfi_iso10926(&cfi_str)?;

    FuturesContract::new(
        instrument_id,
        instrument_id.symbol,
        asset_class.unwrap_or(AssetClass::Commodity),
        underlying,
        msg.activation,
        msg.expiration,
        currency,
        currency.precision,
        decode_min_price_increment(msg.min_price_increment, currency)?,
        Quantity::new(1.0, 0)?, // TBD
        Quantity::new(1.0, 0)?, // TBD
        None,                   // TBD
        None,                   // TBD
        None,                   // TBD
        None,                   // TBD
        msg.ts_recv,            // More accurate and reliable timestamp
        ts_init,
    )
}

pub fn decode_futures_spread(
    msg: &dbn::InstrumentDefMsg,
    instrument_id: InstrumentId,
    ts_init: UnixNanos,
) -> Result<FuturesSpread> {
    let currency = Currency::USD(); // TODO: Temporary hard coding of US futures for now
    let cfi_str = unsafe { raw_ptr_to_string(msg.cfi.as_ptr())? };
    let underlying = unsafe { raw_ptr_to_ustr(msg.asset.as_ptr())? };
    let strategy_type = unsafe { raw_ptr_to_ustr(msg.secsubtype.as_ptr())? };
    let (asset_class, _) = parse_cfi_iso10926(&cfi_str)?;

    FuturesSpread::new(
        instrument_id,
        instrument_id.symbol,
        asset_class.unwrap_or(AssetClass::Commodity),
        underlying,
        strategy_type,
        msg.activation,
        msg.expiration,
        currency,
        currency.precision,
        decode_min_price_increment(msg.min_price_increment, currency)?,
        Quantity::new(1.0, 0)?, // TBD
        Quantity::new(1.0, 0)?, // TBD
        None,                   // TBD
        None,                   // TBD
        None,                   // TBD
        None,                   // TBD
        msg.ts_recv,            // More accurate and reliable timestamp
        ts_init,
    )
}

pub fn decode_options_contract(
    msg: &dbn::InstrumentDefMsg,
    instrument_id: InstrumentId,
    ts_init: UnixNanos,
) -> Result<OptionsContract> {
    let currency_str = unsafe { raw_ptr_to_string(msg.currency.as_ptr())? };
    let cfi_str = unsafe { raw_ptr_to_string(msg.cfi.as_ptr())? };
    let asset_class_opt = match instrument_id.venue.value.as_str() {
        "OPRA" => Some(AssetClass::Equity),
        _ => {
            let (asset_class, _) = parse_cfi_iso10926(&cfi_str)?;
            asset_class
        }
    };
    let underlying = unsafe { raw_ptr_to_ustr(msg.underlying.as_ptr())? };
    let currency = Currency::from_str(&currency_str)?;

    OptionsContract::new(
        instrument_id,
        instrument_id.symbol,
        asset_class_opt.unwrap_or(AssetClass::Commodity),
        underlying,
        parse_option_kind(msg.instrument_class)?,
        msg.activation,
        msg.expiration,
        Price::from_raw(msg.strike_price, currency.precision)?,
        currency,
        currency.precision,
        decode_min_price_increment(msg.min_price_increment, currency)?,
        Quantity::new(1.0, 0)?, // TBD
        Quantity::new(1.0, 0)?, // TBD
        None,                   // TBD
        None,                   // TBD
        None,                   // TBD
        None,                   // TBD
        msg.ts_recv,            // More accurate and reliable timestamp
        ts_init,
    )
}

pub fn decode_options_spread(
    msg: &dbn::InstrumentDefMsg,
    instrument_id: InstrumentId,
    ts_init: UnixNanos,
) -> Result<OptionsSpread> {
    let currency_str = unsafe { raw_ptr_to_string(msg.currency.as_ptr())? };
    let cfi_str = unsafe { raw_ptr_to_string(msg.cfi.as_ptr())? };
    let asset_class_opt = match instrument_id.venue.value.as_str() {
        "OPRA" => Some(AssetClass::Equity),
        _ => {
            let (asset_class, _) = parse_cfi_iso10926(&cfi_str)?;
            asset_class
        }
    };
    let underlying = unsafe { raw_ptr_to_ustr(msg.underlying.as_ptr())? };
    let strategy_type = unsafe { raw_ptr_to_ustr(msg.secsubtype.as_ptr())? };
    let currency = Currency::from_str(&currency_str)?;

    OptionsSpread::new(
        instrument_id,
        instrument_id.symbol,
        asset_class_opt.unwrap_or(AssetClass::Commodity),
        underlying,
        strategy_type,
        msg.activation,
        msg.expiration,
        currency,
        currency.precision,
        decode_min_price_increment(msg.min_price_increment, currency)?,
        Quantity::new(1.0, 0)?, // TBD
        Quantity::new(1.0, 0)?, // TBD
        None,                   // TBD
        None,                   // TBD
        None,                   // TBD
        None,                   // TBD
        msg.ts_recv,            // More accurate and reliable timestamp
        ts_init,
    )
}
