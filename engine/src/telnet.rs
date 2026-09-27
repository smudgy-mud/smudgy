use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use encoding_rs::Encoding;
use flate2::Decompress;
use smudgy_protocol::{
    gmcp, msdp, responders,
    telnet::{self, CompressionStart, Side, TelnetParser, TelnetSink, option},
    transcode::Transcode,
};

/// Per-WebSocket-frame budget. Worker isolation protects the window; this
/// bound also prevents one compressed frame monopolizing its own worker.
const MAX_INFLATED_FRAME: usize = 16 * 1024 * 1024;

#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct TelnetOutput {
    pub display: Vec<DisplayEvent>,
    pub replies: Vec<Vec<u8>>,
    pub server_echo: Option<bool>,
    pub gmcp_messages: u64,
}

/// Preserve prompt boundaries in wire order, including within one WebSocket frame.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum DisplayEvent {
    Data(Vec<u8>),
    Prompt,
}

/// Portable policy adapter around Smudgy's production Telnet parser.
pub(crate) struct TelnetBridge {
    parser: TelnetParser,
    protocol: responders::ProtocolState,
    window_size: Arc<AtomicU32>,
    naws_enabled: bool,
    inflater: Option<Box<Decompress>>,
    inflate_buf: Vec<u8>,
    transcode: Transcode,
}

impl TelnetBridge {
    #[cfg(test)]
    pub fn new(columns: u16, rows: u16) -> Self {
        Self::with_encoding(columns, rows, encoding_rs::UTF_8)
    }

    pub fn with_encoding(columns: u16, rows: u16, encoding: &'static Encoding) -> Self {
        let mut parser = TelnetParser::new();
        parser.set_accept_compression(true, false);
        let window_size = Arc::new(AtomicU32::new(responders::pack_dims(columns, rows)));
        Self {
            parser,
            protocol: responders::ProtocolState::new(Arc::clone(&window_size), true),
            window_size,
            naws_enabled: false,
            inflater: None,
            inflate_buf: Vec::new(),
            transcode: Transcode::new(encoding),
        }
    }

    pub fn feed(&mut self, bytes: &[u8]) -> Result<TelnetOutput, String> {
        let mut output = TelnetOutput::default();
        let mut remaining = bytes;
        let mut drain = false;
        let mut inflated = 0usize;
        while !remaining.is_empty() || drain {
            if let Some(inflater) = self.inflater.as_mut() {
                let (consumed, ended) =
                    smudgy_protocol::mccp::inflate_step(inflater, remaining, &mut self.inflate_buf)
                        .map_err(|error| format!("MCCP2 decompression failed: {error}"))?;
                remaining = &remaining[consumed..];
                inflated = inflated.saturating_add(self.inflate_buf.len());
                if inflated > MAX_INFLATED_FRAME {
                    return Err("MCCP2 frame exceeded the decompression budget".into());
                }
                if !self.inflate_buf.is_empty() {
                    let mut sink = EngineSink {
                        output: &mut output,
                        protocol: &mut self.protocol,
                        naws_enabled: &mut self.naws_enabled,
                        transcode: &mut self.transcode,
                    };
                    let consumed = self.parser.receive(&self.inflate_buf, &mut sink);
                    if consumed != self.inflate_buf.len() {
                        return Err("nested MCCP2 stream is not supported".into());
                    }
                    // A nested start marker must not leak into a later plain tail.
                    let _ = self.parser.take_compression_started();
                }
                if ended {
                    self.inflater = None;
                    drain = false;
                } else if consumed == 0 && self.inflate_buf.is_empty() {
                    if !remaining.is_empty() {
                        return Err("MCCP2 decoder made no progress".into());
                    }
                    break;
                } else {
                    drain = remaining.is_empty();
                }
                continue;
            }
            drain = false;
            let mut sink = EngineSink {
                output: &mut output,
                protocol: &mut self.protocol,
                naws_enabled: &mut self.naws_enabled,
                transcode: &mut self.transcode,
            };
            let consumed = self.parser.receive(remaining, &mut sink);
            remaining = &remaining[consumed..];
            if let Some(start) = self.parser.take_compression_started() {
                if start != CompressionStart::Deflate {
                    return Err("unsupported MCCP compression start".into());
                }
                self.inflater = Some(Box::new(Decompress::new(true)));
            } else if consumed == 0 && !remaining.is_empty() {
                return Err("Telnet parser made no progress".into());
            }
        }
        Ok(output)
    }

    pub fn resize(&mut self, columns: u16, rows: u16) -> Option<Vec<u8>> {
        self.window_size
            .store(responders::pack_dims(columns, rows), Ordering::Relaxed);
        if !self.naws_enabled {
            return None;
        }
        let mut reply = Vec::new();
        self.protocol
            .send_naws_if_changed(&mut reply)
            .then_some(reply)
    }

    pub fn encode_line(&mut self, line: &str) -> Result<Vec<u8>, String> {
        let mut bytes = if self.transcode.is_passthrough() {
            line.as_bytes().to_vec()
        } else {
            self.transcode
                .encode_outbound(line)
                .map_err(|error| error.to_string())?
                .to_vec()
        };
        bytes.extend_from_slice(b"\r\n");
        Ok(bytes)
    }
}

struct EngineSink<'a> {
    output: &'a mut TelnetOutput,
    protocol: &'a mut responders::ProtocolState,
    naws_enabled: &'a mut bool,
    transcode: &'a mut Transcode,
}

impl EngineSink<'_> {
    fn reply(&mut self, build: impl FnOnce(&mut Vec<u8>)) {
        let mut bytes = Vec::new();
        build(&mut bytes);
        if !bytes.is_empty() {
            self.output.replies.push(bytes);
        }
    }

    fn on_charset(&mut self, payload: &[u8]) {
        use responders::charset;

        let Some((&code, rest)) = payload.split_first() else {
            return;
        };
        match code {
            charset::REQUEST => {
                self.protocol.reset_charset_request();
                let mut reply = Vec::new();
                let switch_to =
                    charset::answer_request(rest, self.transcode.configured_encoding(), &mut reply);
                self.output.replies.push(reply);
                if let Some(encoding) = switch_to {
                    self.transcode.switch_to(encoding);
                }
            }
            charset::ACCEPTED | charset::REJECTED => {
                if let Some(encoding) = self
                    .protocol
                    .on_charset_answer(payload, self.transcode.configured_encoding())
                {
                    self.transcode.switch_to(encoding);
                }
            }
            charset::TTABLE_IS => {
                self.reply(|reply| {
                    telnet::frame_subnegotiation(
                        option::CHARSET,
                        &[charset::TTABLE_REJECTED],
                        reply,
                    );
                });
                self.protocol.reset_charset_request();
            }
            _ => {}
        }
    }
}

impl TelnetSink for EngineSink<'_> {
    fn on_data(&mut self, data: &[u8]) {
        let data = if self.transcode.is_passthrough() {
            data
        } else {
            self.transcode.decode(data).as_bytes()
        };
        if let Some(DisplayEvent::Data(last)) = self.output.display.last_mut() {
            last.extend_from_slice(data);
        } else {
            self.output.display.push(DisplayEvent::Data(data.to_vec()));
        }
    }

    fn on_prompt(&mut self) {
        self.output.display.push(DisplayEvent::Prompt);
    }

    fn on_send(&mut self, bytes: &[u8]) {
        self.output.replies.push(bytes.to_vec());
    }

    fn on_subnegotiation(&mut self, negotiated_option: u8, payload: &[u8]) {
        match negotiated_option {
            option::TTYPE if payload == [responders::ttype::SEND] => {
                let mut reply = Vec::new();
                self.protocol.on_ttype_send(&mut reply);
                self.output.replies.push(reply);
            }
            option::NEW_ENVIRON if payload.first() == Some(&responders::new_environ::SEND) => {
                let mut reply = Vec::new();
                self.protocol.on_new_environ_send(
                    &payload[1..],
                    self.transcode.encoding().name(),
                    &mut reply,
                );
                self.output.replies.push(reply);
            }
            option::CHARSET => self.on_charset(payload),
            option::GMCP => {
                self.output.gmcp_messages += 1;
                let text = String::from_utf8_lossy(payload);
                let (name, _) = gmcp::split_message(&text);
                if name.eq_ignore_ascii_case("Core.Ping") {
                    self.reply(|reply| gmcp::frame_message("Core.Ping", None, reply));
                }
            }
            _ => {}
        }
    }

    fn on_option(&mut self, side: Side, negotiated_option: u8, enabled: bool) {
        match (side, negotiated_option) {
            (Side::Local, option::NAWS) if enabled => {
                *self.naws_enabled = true;
                let mut reply = Vec::new();
                self.protocol.send_naws(&mut reply);
                self.output.replies.push(reply);
            }
            (Side::Local, option::NAWS) => *self.naws_enabled = false,
            (Side::Local, option::TTYPE) if !enabled => self.protocol.reset_ttype(),
            (Side::Local, option::CHARSET) if enabled => {
                let mut reply = Vec::new();
                self.protocol.send_charset_request(
                    self.transcode.configured_encoding(),
                    self.transcode.encoding(),
                    &mut reply,
                );
                self.output.replies.push(reply);
            }
            (Side::Local, option::CHARSET) => self.protocol.reset_charset_request(),
            (Side::Remote, option::GMCP) if enabled => self.reply(gmcp::frame_handshake),
            (Side::Remote, option::MSDP) if enabled => self.reply(msdp::frame_handshake),
            (Side::Remote, option::ECHO) => self.output.server_echo = Some(enabled),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use smudgy_protocol::telnet::command::{DO, DONT, IAC, WILL};

    #[test]
    fn production_parser_strips_negotiation_and_tracks_echo() {
        let mut telnet = TelnetBridge::new(100, 36);
        let output = telnet
            .feed(&[b'H', b'i', IAC, WILL, option::ECHO, b'!'])
            .unwrap();

        assert_eq!(output.display, [DisplayEvent::Data(b"Hi!".to_vec())]);
        assert_eq!(output.replies, [vec![IAC, DO, option::ECHO]]);
        assert_eq!(output.server_echo, Some(true));
    }

    #[test]
    fn portable_policy_accepts_mccp2_but_declines_mccp4() {
        let mut telnet = TelnetBridge::new(80, 24);
        let output = telnet
            .feed(&[IAC, WILL, option::MCCP2, IAC, WILL, option::MCCPX])
            .unwrap();

        assert_eq!(
            output.replies,
            [vec![IAC, DO, option::MCCP2], vec![IAC, DONT, option::MCCPX]]
        );
    }

    #[test]
    fn resize_only_sends_after_naws_is_negotiated() {
        let mut telnet = TelnetBridge::new(80, 24);
        assert!(telnet.resize(100, 30).is_none());
        telnet.feed(&[IAC, DO, option::NAWS]).unwrap();
        assert!(telnet.resize(120, 40).is_some());
        assert!(telnet.resize(120, 40).is_none());
    }

    #[test]
    fn preserves_ga_and_eor_boundaries_between_data_segments() {
        use smudgy_protocol::telnet::command::{EOR, GA};
        let mut telnet = TelnetBridge::new(80, 24);
        let output = telnet.feed(&[b'>', IAC, GA, b'A', IAC, EOR, b'B']).unwrap();
        assert_eq!(
            output.display,
            [
                DisplayEvent::Data(b">".to_vec()),
                DisplayEvent::Prompt,
                DisplayEvent::Data(b"A".to_vec()),
                DisplayEvent::Prompt,
                DisplayEvent::Data(b"B".to_vec()),
            ]
        );
    }

    #[test]
    fn mccp2_handles_split_start_compressed_negotiation_and_plain_tail() {
        use flate2::{Compression, write::ZlibEncoder};
        use std::io::Write;

        let mut z = ZlibEncoder::new(Vec::new(), Compression::default());
        z.write_all(&[b'A', IAC, WILL, option::ECHO, b'B']).unwrap();
        let compressed = z.finish().unwrap();
        let mut telnet = TelnetBridge::new(80, 24);
        let offer = telnet.feed(&[IAC, WILL, option::MCCP2]).unwrap();
        assert_eq!(offer.replies, [vec![IAC, DO, option::MCCP2]]);
        telnet
            .feed(&[IAC, telnet::command::SB, option::MCCP2, IAC])
            .unwrap();
        let mut tail = vec![telnet::command::SE];
        tail.extend_from_slice(&compressed);
        tail.extend_from_slice(b"C");

        let mut display = Vec::new();
        let mut replies = Vec::new();
        for piece in tail.chunks(3) {
            let output = telnet.feed(piece).unwrap();
            display.extend(output.display);
            replies.extend(output.replies);
        }
        let text: Vec<u8> = display
            .into_iter()
            .flat_map(|event| match event {
                DisplayEvent::Data(bytes) => bytes,
                DisplayEvent::Prompt => Vec::new(),
            })
            .collect();
        assert_eq!(text, b"ABC");
        assert_eq!(replies, [vec![IAC, DO, option::ECHO]]);
    }

    #[test]
    fn mccp2_rejects_corrupt_or_oversized_frames() {
        use flate2::{Compression, write::ZlibEncoder};
        use std::io::Write;

        let marker = [
            IAC,
            WILL,
            option::MCCP2,
            IAC,
            telnet::command::SB,
            option::MCCP2,
            IAC,
            telnet::command::SE,
        ];
        let mut corrupt = TelnetBridge::new(80, 24);
        corrupt.feed(&marker).unwrap();
        assert!(corrupt.feed(b"not a zlib stream").is_err());

        let mut z = ZlibEncoder::new(Vec::new(), Compression::default());
        z.write_all(&vec![b'x'; MAX_INFLATED_FRAME + 1]).unwrap();
        let compressed = z.finish().unwrap();
        let mut oversized = TelnetBridge::new(80, 24);
        oversized.feed(&marker).unwrap();
        assert!(oversized.feed(&compressed).unwrap_err().contains("budget"));
    }

    #[test]
    fn configured_charset_decodes_streaming_text_and_escapes_outbound_iac() {
        let mut telnet = TelnetBridge::with_encoding(80, 24, encoding_rs::WINDOWS_1252);
        let output = telnet.feed(&[b'c', b'a', b'f', 0xE9]).unwrap();
        assert_eq!(
            output.display,
            [DisplayEvent::Data("café".as_bytes().to_vec())]
        );
        assert_eq!(telnet.encode_line("ÿ").unwrap(), [IAC, IAC, b'\r', b'\n']);
    }

    #[test]
    fn charset_answer_switches_before_following_wire_bytes() {
        use smudgy_protocol::responders::charset;
        use smudgy_protocol::telnet::command::{SB, SE};

        let mut telnet = TelnetBridge::with_encoding(80, 24, encoding_rs::WINDOWS_1252);
        let start = telnet.feed(&[IAC, DO, option::CHARSET]).unwrap();
        assert_eq!(start.replies[0], [IAC, WILL, option::CHARSET]);
        let mut message = vec![IAC, SB, option::CHARSET, charset::ACCEPTED];
        message.extend_from_slice(b"UTF-8");
        message.extend_from_slice(&[IAC, SE]);
        message.extend_from_slice("é".as_bytes());
        let output = telnet.feed(&message).unwrap();
        assert_eq!(
            output.display,
            [DisplayEvent::Data("é".as_bytes().to_vec())]
        );
        assert_eq!(telnet.encode_line("ÿ").unwrap(), "ÿ\r\n".as_bytes());
    }
}
