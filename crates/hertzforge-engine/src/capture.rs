//! Capture seam: the boundary the engine hands each raw block through.
//!
//! The engine hands whole blocks of raw, interleaved f32 frames plus the timebase
//! metadata (epoch, starting frame index, and whether a gap precedes the block) to a
//! [`CaptureSink`]. The implementations in this workspace are the in-memory
//! [`VecCaptureSink`] and the test sinks; nothing behind this trait writes to disk.
//!
//! The recording a run keeps on disk is made elsewhere: a test service pushes samples
//! to `hertzforge_capture::CaptureSink`, which writes a raw interleaved f32 stream
//! (`telemetry.f32`) and a JSON sidecar (`capture.json`) on its own writer thread.
//! There is no HDF5 writer. A run folder is converted to a `.hforge` file afterwards,
//! by the Python function `hertzforge.io.run_convert.convert`.
//!
//! A sink that cannot take a block returns [`CaptureError::WouldBlock`]; the scheduler
//! drops that block and goes on, never blocking the loop. The scheduler does not mark
//! the dropped frames: they are left out of the run report's `frames_captured`, and a
//! sink that needs to record the gap must do so itself. [`CaptureError::Fatal`] ends
//! the run.

use hertzforge_channel_table::ChannelTable;

/// Metadata accompanying a written block so the capture layer can index it against the
/// device timebase (epoch boundaries and gaps must be representable).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockMeta {
    /// Device epoch this block belongs to (bumps on a device reset).
    pub epoch: u32,
    /// Global monotonic frame index of the FIRST frame in this block.
    pub start_frame: u64,
    /// Number of frames in this block.
    pub n_frames: u64,
    /// True if a time-gap precedes this block (dropped samples), so the writer marks a
    /// gap in the record index rather than presenting the data as contiguous.
    pub gap_before: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CaptureError {
    /// The sink is full / disk is behind. The scheduler drops the block and continues,
    /// without marking it (the block is left out of `frames_captured`); it never
    /// blocks the RT thread on the writer.
    WouldBlock,
    /// A fatal writer error (disk full, file closed). The run should stop.
    Fatal(String),
}

/// The disk-streaming boundary. A block is `n_frames * frame_len` interleaved f32s.
/// Implementations must be non-blocking from the caller's perspective (return
/// `WouldBlock` rather than parking the RT thread).
pub trait CaptureSink: Send {
    /// Called once at `configure()` so the sink can author its layout from the channel
    /// table (channel count, names, units). Default: no-op.
    fn on_configure(&mut self, _table: &ChannelTable) -> Result<(), CaptureError> {
        Ok(())
    }

    /// Write one block of raw interleaved frames with its timebase metadata.
    fn write_block(&mut self, samples: &[f32], meta: BlockMeta) -> Result<(), CaptureError>;

    /// Flush and finalize (close the file / commit the index). Default: no-op.
    fn finalize(&mut self) -> Result<(), CaptureError> {
        Ok(())
    }
}

/// In-memory capture sink for tests: accumulates every block plus its metadata so a
/// test can assert exactly what flowed backend -> ring -> processing -> capture.
#[derive(Debug, Default)]
pub struct VecCaptureSink {
    pub samples: Vec<f32>,
    pub blocks: Vec<BlockMeta>,
    pub configured: bool,
    pub finalized: bool,
}

impl VecCaptureSink {
    pub fn new() -> Self {
        Self::default()
    }

    /// Total frames captured across all blocks.
    pub fn total_frames(&self) -> u64 {
        self.blocks.iter().map(|b| b.n_frames).sum()
    }
}

impl CaptureSink for VecCaptureSink {
    fn on_configure(&mut self, _table: &ChannelTable) -> Result<(), CaptureError> {
        self.configured = true;
        Ok(())
    }

    fn write_block(&mut self, samples: &[f32], meta: BlockMeta) -> Result<(), CaptureError> {
        self.samples.extend_from_slice(samples);
        self.blocks.push(meta);
        Ok(())
    }

    fn finalize(&mut self) -> Result<(), CaptureError> {
        self.finalized = true;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vec_sink_accumulates_blocks_and_meta() {
        let mut sink = VecCaptureSink::new();
        sink.on_configure(&ChannelTable::new()).unwrap();
        sink.write_block(
            &[1.0, 2.0, 3.0, 4.0],
            BlockMeta {
                epoch: 0,
                start_frame: 0,
                n_frames: 2,
                gap_before: false,
            },
        )
        .unwrap();
        sink.write_block(
            &[5.0, 6.0],
            BlockMeta {
                epoch: 1,
                start_frame: 100,
                n_frames: 1,
                gap_before: true,
            },
        )
        .unwrap();
        sink.finalize().unwrap();

        assert!(sink.configured);
        assert!(sink.finalized);
        assert_eq!(sink.samples, vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0]);
        assert_eq!(sink.total_frames(), 3);
        assert_eq!(sink.blocks[1].epoch, 1);
        assert!(sink.blocks[1].gap_before);
    }
}
