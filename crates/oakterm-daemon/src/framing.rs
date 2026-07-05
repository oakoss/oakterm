//! Frame-level socket I/O shared by the handshake and client loop.

use bytes::BytesMut;
use oakterm_protocol::frame::{Frame, FrameCodec};
use std::io;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;
use tokio_util::codec::Encoder;

pub(crate) async fn read_frame(stream: &mut UnixStream, buf: &mut BytesMut) -> io::Result<()> {
    let n = stream.read_buf(buf).await?;
    if n == 0 {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "client disconnected",
        ));
    }
    Ok(())
}

pub(crate) async fn write_frame(
    stream: &mut UnixStream,
    codec: &mut FrameCodec,
    buf: &mut BytesMut,
    frame: Frame,
) -> io::Result<()> {
    buf.clear();
    codec.encode(frame, buf)?;
    stream.write_all(buf).await?;
    Ok(())
}
