//! Stream handling for QUIC connections.

#![allow(dead_code)]

use crate::error::Error;

/// A send stream for writing data.
pub struct SendStream {
    stream_id: u64,
    // tquic connection and stream state will be added here
}

impl SendStream {
    /// Create a new send stream.
    pub(crate) fn new(stream_id: u64) -> Self {
        Self { stream_id }
    }

    /// Get the stream ID.
    pub fn stream_id(&self) -> u64 {
        self.stream_id
    }

    /// Write data to the stream.
    pub async fn write(&mut self, data: &[u8]) -> Result<usize, Error> {
        // TODO: Implement with tquic
        Ok(data.len())
    }

    /// Write all data to the stream.
    pub async fn write_all(&mut self, _data: &[u8]) -> Result<(), Error> {
        // TODO: Implement with tquic
        Ok(())
    }

    /// Finish the stream (send FIN).
    pub async fn finish(&mut self) -> Result<(), Error> {
        // TODO: Implement with tquic
        Ok(())
    }

    /// Reset the stream with an error code.
    pub fn reset(&mut self, _error_code: u64) -> Result<(), Error> {
        // TODO: Implement with tquic
        Ok(())
    }
}

/// A receive stream for reading data.
pub struct RecvStream {
    stream_id: u64,
    // tquic connection and stream state will be added here
}

impl RecvStream {
    /// Create a new receive stream.
    pub(crate) fn new(stream_id: u64) -> Self {
        Self { stream_id }
    }

    /// Get the stream ID.
    pub fn stream_id(&self) -> u64 {
        self.stream_id
    }

    /// Read data from the stream.
    pub async fn read(&mut self, _buf: &mut [u8]) -> Result<Option<usize>, Error> {
        // TODO: Implement with tquic
        // Returns None on FIN, Some(n) for data, Err on error
        Ok(Some(0))
    }

    /// Stop reading from the stream with an error code.
    pub fn stop(&mut self, _error_code: u64) -> Result<(), Error> {
        // TODO: Implement with tquic
        Ok(())
    }
}

/// A bidirectional stream.
pub struct BiStream {
    /// The send half of the stream.
    pub send: SendStream,
    /// The receive half of the stream.
    pub recv: RecvStream,
}

impl BiStream {
    /// Create a new bidirectional stream.
    pub(crate) fn new(stream_id: u64) -> Self {
        Self {
            send: SendStream::new(stream_id),
            recv: RecvStream::new(stream_id),
        }
    }

    /// Get the stream ID.
    pub fn stream_id(&self) -> u64 {
        self.send.stream_id
    }
}
