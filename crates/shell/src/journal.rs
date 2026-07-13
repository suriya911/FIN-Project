//! The journal: an append-only binary log of input events, prefixed with
//! the book config. A journal IS a market session — replay it anywhere
//! and the engine reproduces every output and the exact final state hash.
//! ("Production bug? Ship me the log.")

use crate::codec::{decode_input, encode_input, CodecError, INPUT_RECORD};
use std::fs::File;
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::Path;
use tessera_core::{BookConfig, EventBuffer, InputEvent, OrderBook};

const MAGIC: &[u8; 8] = b"TSRAJRN1";
pub const HEADER: usize = 32;

pub struct JournalWriter {
    w: BufWriter<File>,
    events: u64,
}

impl JournalWriter {
    pub fn create<P: AsRef<Path>>(path: P, cfg: &BookConfig) -> io::Result<Self> {
        let mut w = BufWriter::new(File::create(path)?);
        w.write_all(MAGIC)?;
        w.write_all(&cfg.min_price.to_le_bytes())?;
        w.write_all(&cfg.tick_size.to_le_bytes())?;
        w.write_all(&cfg.num_levels.to_le_bytes())?;
        w.write_all(&cfg.max_live_orders.to_le_bytes())?;
        Ok(JournalWriter { w, events: 0 })
    }

    pub fn append(&mut self, ev: &InputEvent) -> io::Result<()> {
        let mut rec = [0u8; INPUT_RECORD];
        encode_input(ev, &mut rec);
        self.w.write_all(&rec)?;
        self.events += 1;
        Ok(())
    }

    pub fn finish(mut self) -> io::Result<u64> {
        self.w.flush()?;
        Ok(self.events)
    }
}

pub struct JournalReader {
    r: BufReader<File>,
    pub cfg: BookConfig,
}

impl JournalReader {
    pub fn open<P: AsRef<Path>>(path: P) -> io::Result<Self> {
        let mut r = BufReader::new(File::open(path)?);
        let mut header = [0u8; HEADER];
        r.read_exact(&mut header)?;
        if &header[0..8] != MAGIC {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "not a tessera journal",
            ));
        }
        let cfg = BookConfig {
            min_price: i64::from_le_bytes(header[8..16].try_into().unwrap()),
            tick_size: i64::from_le_bytes(header[16..24].try_into().unwrap()),
            num_levels: u32::from_le_bytes(header[24..28].try_into().unwrap()),
            max_live_orders: u32::from_le_bytes(header[28..32].try_into().unwrap()),
        };
        if cfg.tick_size < 1 || cfg.num_levels == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "corrupt journal header",
            ));
        }
        Ok(JournalReader { r, cfg })
    }

    /// Next event, `Ok(None)` at clean EOF, error on truncation/corruption.
    pub fn next_event(&mut self) -> io::Result<Option<InputEvent>> {
        let mut rec = [0u8; INPUT_RECORD];
        match self.r.read_exact(&mut rec) {
            Ok(()) => decode_input(&rec)
                .map(Some)
                .map_err(|CodecError(m)| io::Error::new(io::ErrorKind::InvalidData, m)),
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => Ok(None),
            Err(e) => Err(e),
        }
    }
}

/// Result of replaying a journal through a fresh engine.
pub struct ReplayResult {
    pub events: u64,
    pub outputs: u64,
    /// FNV fingerprint of the full output stream.
    pub output_hash: u64,
    /// Canonical semantic hash of the final book.
    pub state_hash: u64,
}

/// Replay a journal file through a fresh engine.
pub fn replay<P: AsRef<Path>>(path: P) -> io::Result<ReplayResult> {
    let mut r = JournalReader::open(path)?;
    let mut book = OrderBook::new(r.cfg);
    let mut buf = EventBuffer::for_book(&r.cfg);
    let mut out_hash = tessera_core::hash::Fnv1a::new();
    let (mut events, mut outputs) = (0u64, 0u64);
    while let Some(ev) = r.next_event()? {
        buf.clear();
        book.apply(ev, &mut buf);
        events += 1;
        outputs += buf.len() as u64;
        let mut rec = [0u8; crate::codec::OUTPUT_RECORD];
        for out in buf.as_slice() {
            crate::codec::encode_output(out, &mut rec);
            out_hash.write_bytes(&rec);
        }
    }
    Ok(ReplayResult {
        events,
        outputs,
        output_hash: out_hash.finish(),
        state_hash: book.state_hash(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_replay_roundtrip_matches_live_run() {
        let sc = tessera_sim::SimConfig {
            seed: 5,
            events: 50_000,
            ..Default::default()
        };
        let sim = tessera_sim::run(sc);

        let path = std::env::temp_dir().join("tessera_journal_test.bin");
        let mut w = JournalWriter::create(&path, &sc.book).unwrap();
        for ev in &sim.log {
            w.append(ev).unwrap();
        }
        let n = w.finish().unwrap();
        assert_eq!(n as usize, sim.log.len());

        // Replaying the journal reproduces the live session's final state
        // hash exactly, and twice gives identical fingerprints.
        let a = replay(&path).unwrap();
        let b = replay(&path).unwrap();
        assert_eq!(a.events as usize, sim.log.len());
        assert_eq!(a.state_hash, sim.final_state_hash);
        assert_eq!(a.output_hash, b.output_hash);
        assert_eq!(a.state_hash, b.state_hash);
        std::fs::remove_file(&path).ok();
    }
}
