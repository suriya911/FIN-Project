//! Wire codec: fixed-size little-endian records for input and output
//! events. Fixed size keeps the journal seekable (event N lives at a
//! computable offset) and the encoder allocation-free.
//!
//! Decoding NEVER panics: malformed bytes produce `CodecError`, because
//! logs cross machine boundaries and the shell must survive corruption.

use tessera_core::{
    CancelReason, InputEvent, OrderId, OutputEvent, Price, Qty, RejectReason, SelfTradePrevention,
    Seq, Side, TimeInForce, TraderId,
};

pub const INPUT_RECORD: usize = 40;
pub const OUTPUT_RECORD: usize = 48;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodecError(pub &'static str);

impl std::fmt::Display for CodecError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "codec error: {}", self.0)
    }
}
impl std::error::Error for CodecError {}

// Input layout (40 bytes):
//   [0]      tag: 0=New 1=Cancel 2=Modify
//   [1]      side (New)            else 0
//   [2]      tif (New)             else 0
//   [3]      stp (New)             else 0
//   [4..8]   trader u32
//   [8..16]  seq u64
//   [16..24] order_id u64
//   [24..32] price/new_price i64   else 0
//   [32..40] qty/new_qty u64       else 0

pub fn encode_input(ev: &InputEvent, out: &mut [u8; INPUT_RECORD]) {
    out.fill(0);
    match *ev {
        InputEvent::New {
            seq,
            order_id,
            trader,
            side,
            price,
            qty,
            tif,
            stp,
        } => {
            out[0] = 0;
            out[1] = side.to_u8();
            out[2] = match tif {
                TimeInForce::Gtc => 0,
                TimeInForce::Ioc => 1,
                TimeInForce::Fok => 2,
            };
            out[3] = stp.to_u8();
            out[4..8].copy_from_slice(&trader.0.to_le_bytes());
            out[8..16].copy_from_slice(&seq.0.to_le_bytes());
            out[16..24].copy_from_slice(&order_id.0.to_le_bytes());
            out[24..32].copy_from_slice(&price.0.to_le_bytes());
            out[32..40].copy_from_slice(&qty.0.to_le_bytes());
        }
        InputEvent::Cancel {
            seq,
            order_id,
            trader,
        } => {
            out[0] = 1;
            out[4..8].copy_from_slice(&trader.0.to_le_bytes());
            out[8..16].copy_from_slice(&seq.0.to_le_bytes());
            out[16..24].copy_from_slice(&order_id.0.to_le_bytes());
        }
        InputEvent::Modify {
            seq,
            order_id,
            trader,
            new_price,
            new_qty,
        } => {
            out[0] = 2;
            out[4..8].copy_from_slice(&trader.0.to_le_bytes());
            out[8..16].copy_from_slice(&seq.0.to_le_bytes());
            out[16..24].copy_from_slice(&order_id.0.to_le_bytes());
            out[24..32].copy_from_slice(&new_price.0.to_le_bytes());
            out[32..40].copy_from_slice(&new_qty.0.to_le_bytes());
        }
    }
}

pub fn decode_input(b: &[u8; INPUT_RECORD]) -> Result<InputEvent, CodecError> {
    let u32le = |r: std::ops::Range<usize>| u32::from_le_bytes(b[r].try_into().unwrap());
    let u64le = |r: std::ops::Range<usize>| u64::from_le_bytes(b[r].try_into().unwrap());
    let i64le = |r: std::ops::Range<usize>| i64::from_le_bytes(b[r].try_into().unwrap());
    let seq = Seq(u64le(8..16));
    let order_id = OrderId(u64le(16..24));
    let trader = TraderId(u32le(4..8));
    match b[0] {
        0 => Ok(InputEvent::New {
            seq,
            order_id,
            trader,
            side: match b[1] {
                0 => Side::Bid,
                1 => Side::Ask,
                _ => return Err(CodecError("bad side")),
            },
            price: Price(i64le(24..32)),
            qty: Qty(u64le(32..40)),
            tif: match b[2] {
                0 => TimeInForce::Gtc,
                1 => TimeInForce::Ioc,
                2 => TimeInForce::Fok,
                _ => return Err(CodecError("bad tif")),
            },
            stp: match b[3] {
                0 => SelfTradePrevention::None,
                1 => SelfTradePrevention::CancelResting,
                2 => SelfTradePrevention::CancelAggressor,
                3 => SelfTradePrevention::CancelBoth,
                _ => return Err(CodecError("bad stp")),
            },
        }),
        1 => Ok(InputEvent::Cancel {
            seq,
            order_id,
            trader,
        }),
        2 => Ok(InputEvent::Modify {
            seq,
            order_id,
            trader,
            new_price: Price(i64le(24..32)),
            new_qty: Qty(u64le(32..40)),
        }),
        _ => Err(CodecError("bad input tag")),
    }
}

// Output layout (48 bytes):
//   [0]      tag: 0=Ack 1=Fill 2=Cancelled 3=Rejected
//   [1]      taker_side (Fill) / reason (Cancelled, Rejected)
//   [2..8]   reserved
//   [8..16]  seq u64
//   [16..24] order_id / taker u64
//   [24..32] maker u64 (Fill)     else 0
//   [32..40] price i64 (Fill)     else 0
//   [40..48] qty/remaining u64    else 0

pub fn encode_output(ev: &OutputEvent, out: &mut [u8; OUTPUT_RECORD]) {
    out.fill(0);
    match *ev {
        OutputEvent::Ack { seq, order_id } => {
            out[0] = 0;
            out[8..16].copy_from_slice(&seq.0.to_le_bytes());
            out[16..24].copy_from_slice(&order_id.0.to_le_bytes());
        }
        OutputEvent::Fill {
            seq,
            taker,
            maker,
            price,
            qty,
            taker_side,
        } => {
            out[0] = 1;
            out[1] = taker_side.to_u8();
            out[8..16].copy_from_slice(&seq.0.to_le_bytes());
            out[16..24].copy_from_slice(&taker.0.to_le_bytes());
            out[24..32].copy_from_slice(&maker.0.to_le_bytes());
            out[32..40].copy_from_slice(&price.0.to_le_bytes());
            out[40..48].copy_from_slice(&qty.0.to_le_bytes());
        }
        OutputEvent::Cancelled {
            seq,
            order_id,
            remaining,
            reason,
        } => {
            out[0] = 2;
            out[1] = match reason {
                CancelReason::User => 0,
                CancelReason::Ioc => 1,
                CancelReason::SelfTradePrevention => 2,
            };
            out[8..16].copy_from_slice(&seq.0.to_le_bytes());
            out[16..24].copy_from_slice(&order_id.0.to_le_bytes());
            out[40..48].copy_from_slice(&remaining.0.to_le_bytes());
        }
        OutputEvent::Rejected {
            seq,
            order_id,
            reason,
        } => {
            out[0] = 3;
            out[1] = match reason {
                RejectReason::DuplicateOrderId => 0,
                RejectReason::UnknownOrderId => 1,
                RejectReason::PriceOutOfBounds => 2,
                RejectReason::ZeroQty => 3,
                RejectReason::FokUnfillable => 4,
                RejectReason::ArenaFull => 5,
                RejectReason::WrongTrader => 6,
            };
            out[8..16].copy_from_slice(&seq.0.to_le_bytes());
            out[16..24].copy_from_slice(&order_id.0.to_le_bytes());
        }
    }
}

pub fn decode_output(b: &[u8; OUTPUT_RECORD]) -> Result<OutputEvent, CodecError> {
    let u64le = |r: std::ops::Range<usize>| u64::from_le_bytes(b[r].try_into().unwrap());
    let i64le = |r: std::ops::Range<usize>| i64::from_le_bytes(b[r].try_into().unwrap());
    let seq = Seq(u64le(8..16));
    let id = OrderId(u64le(16..24));
    match b[0] {
        0 => Ok(OutputEvent::Ack { seq, order_id: id }),
        1 => Ok(OutputEvent::Fill {
            seq,
            taker: id,
            maker: OrderId(u64le(24..32)),
            price: Price(i64le(32..40)),
            qty: Qty(u64le(40..48)),
            taker_side: match b[1] {
                0 => Side::Bid,
                1 => Side::Ask,
                _ => return Err(CodecError("bad taker_side")),
            },
        }),
        2 => Ok(OutputEvent::Cancelled {
            seq,
            order_id: id,
            remaining: Qty(u64le(40..48)),
            reason: match b[1] {
                0 => CancelReason::User,
                1 => CancelReason::Ioc,
                2 => CancelReason::SelfTradePrevention,
                _ => return Err(CodecError("bad cancel reason")),
            },
        }),
        3 => Ok(OutputEvent::Rejected {
            seq,
            order_id: id,
            reason: match b[1] {
                0 => RejectReason::DuplicateOrderId,
                1 => RejectReason::UnknownOrderId,
                2 => RejectReason::PriceOutOfBounds,
                3 => RejectReason::ZeroQty,
                4 => RejectReason::FokUnfillable,
                5 => RejectReason::ArenaFull,
                6 => RejectReason::WrongTrader,
                _ => return Err(CodecError("bad reject reason")),
            },
        }),
        _ => Err(CodecError("bad output tag")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_roundtrip() {
        let evs = [
            InputEvent::New {
                seq: Seq(7),
                order_id: OrderId(u64::MAX),
                trader: TraderId(3),
                side: Side::Ask,
                price: Price(-42),
                qty: Qty(u64::MAX / 3),
                tif: TimeInForce::Fok,
                stp: SelfTradePrevention::CancelBoth,
            },
            InputEvent::Cancel {
                seq: Seq(8),
                order_id: OrderId(0),
                trader: TraderId(u32::MAX),
            },
            InputEvent::Modify {
                seq: Seq(9),
                order_id: OrderId(1),
                trader: TraderId(0),
                new_price: Price(i64::MIN + 1),
                new_qty: Qty(1),
            },
        ];
        let mut buf = [0u8; INPUT_RECORD];
        for ev in evs {
            encode_input(&ev, &mut buf);
            assert_eq!(decode_input(&buf).unwrap(), ev);
        }
    }

    #[test]
    fn output_roundtrip() {
        let evs = [
            OutputEvent::Ack {
                seq: Seq(1),
                order_id: OrderId(2),
            },
            OutputEvent::Fill {
                seq: Seq(2),
                taker: OrderId(3),
                maker: OrderId(4),
                price: Price(-5),
                qty: Qty(6),
                taker_side: Side::Ask,
            },
            OutputEvent::Cancelled {
                seq: Seq(3),
                order_id: OrderId(7),
                remaining: Qty(8),
                reason: CancelReason::SelfTradePrevention,
            },
            OutputEvent::Rejected {
                seq: Seq(4),
                order_id: OrderId(9),
                reason: RejectReason::WrongTrader,
            },
        ];
        let mut buf = [0u8; OUTPUT_RECORD];
        for ev in evs {
            encode_output(&ev, &mut buf);
            assert_eq!(decode_output(&buf).unwrap(), ev);
        }
    }

    #[test]
    fn garbage_decodes_to_error_not_panic() {
        let mut b = [0xFFu8; INPUT_RECORD];
        assert!(decode_input(&b).is_err());
        b[0] = 0; // New with garbage side/tif/stp
        assert!(decode_input(&b).is_err());
        let o = [0xFFu8; OUTPUT_RECORD];
        assert!(decode_output(&o).is_err());
    }
}
