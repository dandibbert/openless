//! AssemblyAI v3 streaming: authenticated WebSocket, raw PCM, turn snapshots,
//! and explicit Terminate/Termination shutdown (sessions are billed while open).

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use parking_lot::Mutex;
use serde_json::{json, Value};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, oneshot, Mutex as AsyncMutex};
use tokio_tungstenite::tungstenite::{client::IntoClientRequest, Message};
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};

use crate::config::{TaskSpawner, TokioTaskSpawner};
use crate::ports::{TextStreamChunk, TextStreamSink};

use super::{qwen_realtime::join_segments, AudioConsumer, RawTranscript};

pub const PROVIDER_ID: &str = "assemblyai";
pub const DEFAULT_ENDPOINT: &str = "wss://streaming.assemblyai.com/v3/ws";
pub const DEFAULT_MODEL: &str = "universal-3-6-pro";
pub const MODELS: &[&str] = &[
    DEFAULT_MODEL,
    "universal-3-5-pro",
    "universal-streaming-english",
    "universal-streaming-multilingual",
];
const AUDIO_CHUNK_BYTES: usize = 3_200; // 100 ms, 16 kHz mono PCM16.
const MIN_AUDIO_BYTES: usize = 1_600; // AssemblyAI requires at least 50 ms per frame.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const WRITE_TIMEOUT: Duration = Duration::from_secs(5);

type WsStream = WebSocketStream<MaybeTlsStream<TcpStream>>;
type SharedWriter = Arc<AsyncMutex<Option<futures_util::stream::SplitSink<WsStream, Message>>>>;
type FinalResult = Result<RawTranscript, AssemblyAiError>;

#[derive(Clone)]
pub struct AssemblyAiCredentials {
    pub api_key: String,
    pub endpoint: String,
    pub model: String,
    pub terms: Vec<String>,
}

impl AssemblyAiCredentials {
    pub fn normalized_model(&self) -> &str {
        match self.model.trim() {
            "" => DEFAULT_MODEL,
            model => model,
        }
    }

    fn request(
        &self,
    ) -> Result<tokio_tungstenite::tungstenite::http::Request<()>, AssemblyAiError> {
        if self.api_key.trim().is_empty() {
            return Err(AssemblyAiError::CredentialsMissing);
        }
        let endpoint = match self.endpoint.trim() {
            "" => DEFAULT_ENDPOINT,
            endpoint => endpoint,
        };
        let mut url = url::Url::parse(endpoint).map_err(|_| AssemblyAiError::InvalidEndpoint)?;
        if !matches!(url.scheme(), "ws" | "wss") || url.host_str().is_none() {
            return Err(AssemblyAiError::InvalidEndpoint);
        }
        // Preserve proxy/region options, but never let custom format parameters
        // disagree with the recorder or put a permanent API key in the URL.
        let extra: Vec<(String, String)> = url
            .query_pairs()
            .filter(|(key, _)| {
                !matches!(
                    key.as_ref(),
                    "sample_rate"
                        | "encoding"
                        | "speech_model"
                        | "format_turns"
                        | "keyterms_prompt"
                        | "token"
                        | "api_key"
                )
            })
            .map(|(key, value)| (key.into_owned(), value.into_owned()))
            .collect();
        url.set_query(None);
        {
            let mut query = url.query_pairs_mut();
            query.extend_pairs(extra);
            query.append_pair("sample_rate", "16000");
            query.append_pair("encoding", "pcm_s16le");
            query.append_pair("speech_model", self.normalized_model());
            if self.normalized_model().starts_with("universal-streaming-") {
                query.append_pair("format_turns", "true");
            }
            let mut terms = Vec::new();
            for term in &self.terms {
                let term = term.trim();
                if !term.is_empty() && term.chars().count() <= 50 && !terms.contains(&term) {
                    terms.push(term);
                }
                if terms.len() == 100 {
                    break;
                }
            }
            if !terms.is_empty() {
                query.append_pair("keyterms_prompt", &json!(terms).to_string());
            }
        }
        let mut request = url
            .as_str()
            .into_client_request()
            .map_err(|_| AssemblyAiError::InvalidEndpoint)?;
        let mut authorization = self
            .api_key
            .trim()
            .parse::<tokio_tungstenite::tungstenite::http::HeaderValue>()
            .map_err(|_| AssemblyAiError::CredentialsMissing)?;
        authorization.set_sensitive(true);
        request.headers_mut().insert("Authorization", authorization);
        Ok(request)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum AssemblyAiError {
    #[error("AssemblyAI API key missing or invalid")]
    CredentialsMissing,
    #[error("AssemblyAI endpoint must be a valid ws:// or wss:// URL")]
    InvalidEndpoint,
    #[error("AssemblyAI connection failed: {0}")]
    Connection(String),
    #[error("AssemblyAI task failed: {0}")]
    Task(String),
    #[error("AssemblyAI {0} timed out")]
    Timeout(&'static str),
    #[error("AssemblyAI session closed before Termination")]
    Closed,
}

enum SendItem {
    Audio(Vec<u8>),
    Finish(oneshot::Sender<Result<(), AssemblyAiError>>),
}

#[derive(Default)]
struct Turn {
    text: String,
    final_turn: bool,
    formatted: bool,
}

#[derive(Default)]
struct State {
    audio: Vec<u8>,
    bytes_received: u64,
    bytes_written: u64,
    started: bool,
    finishing: bool,
    termination_sent: bool,
    finished: bool,
    turns: BTreeMap<u64, Turn>,
    ready_tx: Option<oneshot::Sender<Result<(), String>>>,
    final_tx: Option<oneshot::Sender<FinalResult>>,
    send_tx: Option<mpsc::UnboundedSender<SendItem>>,
}

pub struct AssemblyAiStreamingASR {
    credentials: AssemblyAiCredentials,
    task_spawner: Arc<dyn TaskSpawner>,
    state: Mutex<State>,
    writer: SharedWriter,
    final_rx: Mutex<Option<oneshot::Receiver<FinalResult>>>,
    partial_sink: Mutex<Option<Arc<dyn TextStreamSink>>>,
}

impl AssemblyAiStreamingASR {
    pub fn new(credentials: AssemblyAiCredentials) -> Self {
        Self::with_task_spawner(credentials, Arc::new(TokioTaskSpawner))
    }

    pub fn with_task_spawner(
        credentials: AssemblyAiCredentials,
        task_spawner: Arc<dyn TaskSpawner>,
    ) -> Self {
        Self {
            credentials,
            task_spawner,
            state: Mutex::new(State::default()),
            writer: Arc::new(AsyncMutex::new(None)),
            final_rx: Mutex::new(None),
            partial_sink: Mutex::new(None),
        }
    }

    pub fn set_partial_sink(&self, sink: Arc<dyn TextStreamSink>) {
        *self.partial_sink.lock() = Some(sink);
    }

    pub async fn open_session(self: &Arc<Self>) -> Result<(), AssemblyAiError> {
        let request = self.credentials.request()?;
        let (ws, _) = tokio::time::timeout(CONNECT_TIMEOUT, connect_async(request))
            .await
            .map_err(|_| AssemblyAiError::Timeout("connection"))?
            .map_err(|error| AssemblyAiError::Connection(error.to_string()))?;
        let (write, mut read) = ws.split();
        *self.writer.lock().await = Some(write);
        let (ready_tx, ready_rx) = oneshot::channel();
        let (final_tx, final_rx) = oneshot::channel();
        let (send_tx, mut send_rx) = mpsc::unbounded_channel();
        let cancelled = {
            let mut state = self.state.lock();
            if state.finished {
                true
            } else {
                state.ready_tx = Some(ready_tx);
                state.final_tx = Some(final_tx);
                state.send_tx = Some(send_tx);
                false
            }
        };
        if cancelled {
            close_writer(&self.writer).await;
            return Err(AssemblyAiError::Closed);
        }
        *self.final_rx.lock() = Some(final_rx);

        let writer = Arc::clone(&self.writer);
        let weak = Arc::downgrade(self);
        self.task_spawner.spawn(Box::pin(async move {
            let mut next_send = None;
            while let Some(item) = send_rx.recv().await {
                let (message, done) = match item {
                    SendItem::Audio(bytes) => {
                        super::streaming::pace_audio(&mut next_send, bytes.len()).await;
                        if weak.upgrade().is_none_or(|this| this.state.lock().finished) {
                            break;
                        }
                        (Message::Binary(bytes.into()), None)
                    }
                    SendItem::Finish(done) => {
                        if let Some(this) = weak.upgrade() {
                            this.state.lock().termination_sent = true;
                        }
                        (
                            Message::Text(json!({"type": "Terminate"}).to_string().into()),
                            Some(done),
                        )
                    }
                };
                let bytes = match &message {
                    Message::Binary(bytes) => bytes.len() as u64,
                    _ => 0,
                };
                let result = send_message(&writer, message).await;
                let failed = result.is_err();
                if failed {
                    if let Some(this) = weak.upgrade() {
                        this.finish(Err(AssemblyAiError::Task("audio send failed".into())));
                    }
                } else if let Some(this) = weak.upgrade() {
                    this.state.lock().bytes_written += bytes;
                }
                if let Some(done) = done {
                    let _ = done.send(result);
                }
                if failed {
                    break;
                }
            }
            close_writer(&writer).await;
        }));

        let weak = Arc::downgrade(self);
        self.task_spawner.spawn(Box::pin(async move {
            while let Some(message) = read.next().await {
                let Some(this) = weak.upgrade() else {
                    break;
                };
                match message {
                    Ok(Message::Text(text)) => match serde_json::from_str::<Value>(&text) {
                        Ok(value) => {
                            if !this.handle_response(&value) {
                                return;
                            }
                        }
                        Err(_) => {
                            this.finish(Err(AssemblyAiError::Task("invalid JSON response".into())));
                            return;
                        }
                    },
                    Ok(Message::Close(_)) | Err(_) => {
                        this.finish(Err(AssemblyAiError::Closed));
                        return;
                    }
                    _ => {}
                }
            }
            if let Some(this) = weak.upgrade() {
                this.finish(Err(AssemblyAiError::Closed));
            }
        }));

        match tokio::time::timeout(CONNECT_TIMEOUT, ready_rx).await {
            Ok(Ok(Ok(()))) => Ok(()),
            Ok(Ok(Err(message))) => Err(AssemblyAiError::Task(message)),
            _ => {
                self.cancel();
                Err(AssemblyAiError::Timeout("session Begin"))
            }
        }
    }

    pub async fn send_last_frame(&self) -> Result<(), AssemblyAiError> {
        let done_rx = {
            let mut state = self.state.lock();
            if state.finished || state.finishing {
                return Err(AssemblyAiError::Closed);
            }
            let sender = state.send_tx.clone().ok_or(AssemblyAiError::Closed)?;
            state.finishing = true;
            if !state.audio.is_empty() {
                let mut tail = std::mem::take(&mut state.audio);
                // Pad short/odd tails to legal PCM frames without counting silence
                // in the user's recording duration.
                tail.resize(tail.len().max(MIN_AUDIO_BYTES).next_multiple_of(2), 0);
                sender
                    .send(SendItem::Audio(tail))
                    .map_err(|_| AssemblyAiError::Closed)?;
            }
            let (done_tx, done_rx) = oneshot::channel();
            sender
                .send(SendItem::Finish(done_tx))
                .map_err(|_| AssemblyAiError::Closed)?;
            done_rx
        };
        let pending = {
            let state = self.state.lock();
            state.bytes_received.saturating_sub(state.bytes_written)
        };
        let result = tokio::time::timeout(super::streaming::drain_timeout(pending), done_rx).await;
        match result {
            Ok(Ok(Ok(()))) => Ok(()),
            Ok(Ok(Err(error))) => {
                self.cancel();
                Err(error)
            }
            _ => {
                self.cancel();
                Err(AssemblyAiError::Timeout("Terminate write"))
            }
        }
    }

    pub async fn await_final_result(&self) -> FinalResult {
        let receiver = self.final_rx.lock().take().ok_or(AssemblyAiError::Closed)?;
        let timeout = super::streaming::final_timeout(self.state.lock().bytes_received);
        match tokio::time::timeout(timeout, receiver).await {
            Ok(Ok(result)) => result,
            _ => {
                self.cancel();
                Err(AssemblyAiError::Timeout("Termination"))
            }
        }
    }

    pub fn cancel(&self) {
        // Explicit user cancellation isn't an unexpected disconnect notification.
        self.partial_sink.lock().take();
        self.finish(Err(AssemblyAiError::Closed));
    }

    fn handle_response(&self, value: &Value) -> bool {
        if let Some(error) = value.get("error").and_then(Value::as_str) {
            self.finish(Err(AssemblyAiError::Task(error.to_string())));
            return false;
        }
        match value["type"].as_str().unwrap_or_default() {
            "Begin" => {
                let mut state = self.state.lock();
                if state.finished {
                    return false;
                }
                state.started = true;
                flush_audio(&mut state);
                if let Some(tx) = state.ready_tx.take() {
                    let _ = tx.send(Ok(()));
                }
            }
            "Turn" => {
                let (Some(order), Some(text)) =
                    (value["turn_order"].as_u64(), value["transcript"].as_str())
                else {
                    return true;
                };
                let final_turn = value["end_of_turn"].as_bool().unwrap_or(false);
                let formatted = value["turn_is_formatted"].as_bool().unwrap_or(false);
                let snapshot = {
                    let mut state = self.state.lock();
                    if state.finished {
                        return false;
                    }
                    let turn = state.turns.entry(order).or_default();
                    // Each event is a replacement for one turn, not a delta.
                    // A formatted final may revise a previously finalized turn.
                    if (turn.final_turn && !final_turn) || (turn.formatted && !formatted) {
                        return true;
                    }
                    *turn = Turn {
                        text: text.to_string(),
                        final_turn,
                        formatted,
                    };
                    transcript(&state)
                };
                if let Some(sink) = self.partial_sink.lock().clone() {
                    let _ = sink.publish(TextStreamChunk {
                        text: snapshot,
                        offset: 0,
                    });
                }
            }
            "Termination" => {
                if !self.state.lock().termination_sent {
                    self.finish(Err(AssemblyAiError::Task(
                        "server terminated before audio upload completed".into(),
                    )));
                    return false;
                }
                let result = {
                    let state = self.state.lock();
                    RawTranscript {
                        text: transcript(&state),
                        duration_ms: state.bytes_received / 32,
                    }
                };
                self.finish(Ok(result));
                return false;
            }
            "Error" => {
                let message = value["message"].as_str().unwrap_or("server error");
                self.finish(Err(AssemblyAiError::Task(message.to_string())));
                return false;
            }
            _ => {}
        }
        true
    }

    fn finish(&self, result: FinalResult) {
        let mut state = self.state.lock();
        if state.finished {
            return;
        }
        state.finished = true;
        state.audio.clear();
        state.send_tx.take();
        if let Some(tx) = state.ready_tx.take() {
            let _ = tx.send(Err(result
                .as_ref()
                .err()
                .map(ToString::to_string)
                .unwrap_or_else(|| "session ended before Begin".into())));
        }
        let error = result.as_ref().err().map(|error| {
            crate::BackendError::new(crate::BackendErrorCode::Provider, error.to_string())
        });
        if let Some(tx) = state.final_tx.take() {
            let _ = tx.send(result);
        }
        drop(state);
        if let Some(error) = error {
            if let Some(sink) = self.partial_sink.lock().clone() {
                let _ = sink.fail(error);
            }
        }
        // Also close immediately on cancel, rather than draining queued audio.
        let writer = Arc::clone(&self.writer);
        self.task_spawner.spawn(Box::pin(async move {
            close_writer(&writer).await;
        }));
    }
}

impl AudioConsumer for AssemblyAiStreamingASR {
    fn consume_pcm_chunk(&self, pcm: &[u8]) {
        let mut state = self.state.lock();
        if state.finished || state.finishing {
            return;
        }
        state.bytes_received = state.bytes_received.saturating_add(pcm.len() as u64);
        state.audio.extend_from_slice(pcm);
        if state.started {
            flush_audio(&mut state);
        }
    }
}

impl Drop for AssemblyAiStreamingASR {
    fn drop(&mut self) {
        // Dropping a cancelled open/finish future must not leave a billable socket.
        self.state.get_mut().send_tx.take();
        let writer = Arc::clone(&self.writer);
        self.task_spawner.spawn(Box::pin(async move {
            close_writer(&writer).await;
        }));
    }
}

fn transcript(state: &State) -> String {
    join_segments(
        &state
            .turns
            .values()
            .map(|turn| turn.text.clone())
            .collect::<Vec<_>>(),
    )
}

fn flush_audio(state: &mut State) {
    for chunk in super::streaming::drain_chunks(&mut state.audio, AUDIO_CHUNK_BYTES) {
        if let Some(sender) = &state.send_tx {
            let _ = sender.send(SendItem::Audio(chunk));
        }
    }
}

async fn send_message(writer: &SharedWriter, message: Message) -> Result<(), AssemblyAiError> {
    tokio::time::timeout(WRITE_TIMEOUT, async {
        let mut guard = writer.lock().await;
        guard
            .as_mut()
            .ok_or(AssemblyAiError::Closed)?
            .send(message)
            .await
            .map_err(|error| AssemblyAiError::Connection(error.to_string()))
    })
    .await
    .map_err(|_| AssemblyAiError::Timeout("write"))?
}

async fn close_writer(writer: &SharedWriter) {
    let _ = tokio::time::timeout(WRITE_TIMEOUT, async {
        if let Some(mut writer) = writer.lock().await.take() {
            let _ = writer.close().await;
        }
    })
    .await;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn credentials() -> AssemblyAiCredentials {
        AssemblyAiCredentials {
            api_key: " test-key ".into(),
            endpoint: String::new(),
            model: String::new(),
            terms: vec![],
        }
    }

    fn turn(order: u64, text: &str, final_turn: bool, formatted: bool) -> Value {
        json!({"type": "Turn", "turn_order": order, "transcript": text,
            "end_of_turn": final_turn, "turn_is_formatted": formatted})
    }

    #[test]
    fn request_uses_header_auth_and_recorder_format() {
        let mut creds = credentials();
        creds.endpoint = "wss://streaming.eu.assemblyai.com/v3/ws?sample_rate=8000&encoding=aac&token=secret&speech_model=old&custom=yes".into();
        creds.terms = vec![
            " OpenLess ".into(),
            "".into(),
            "OpenLess".into(),
            "A&B".into(),
            "x".repeat(51),
        ];
        let request = creds.request().unwrap();
        assert_eq!(request.headers()["Authorization"], "test-key");
        assert!(request.headers()["Authorization"].is_sensitive());
        let url = url::Url::parse(&request.uri().to_string()).unwrap();
        let query: BTreeMap<_, _> = url.query_pairs().into_owned().collect();
        assert_eq!(query["sample_rate"], "16000");
        assert_eq!(query["encoding"], "pcm_s16le");
        assert_eq!(query["speech_model"], DEFAULT_MODEL);
        assert_eq!(query["custom"], "yes");
        assert!(!query.contains_key("token"));
        assert!(!query.contains_key("format_turns"));
        assert_eq!(
            serde_json::from_str::<Value>(&query["keyterms_prompt"]).unwrap(),
            json!(["OpenLess", "A&B"])
        );
        assert!(!request.uri().to_string().contains("test-key"));
    }

    #[test]
    fn request_limits_terms_and_formats_legacy_models() {
        let mut creds = credentials();
        creds.model = "universal-streaming-multilingual".into();
        creds.terms = (0..120).map(|i| format!("term-{i}")).collect();
        let request = creds.request().unwrap();
        let url = url::Url::parse(&request.uri().to_string()).unwrap();
        let query: BTreeMap<_, _> = url.query_pairs().into_owned().collect();
        assert_eq!(query["format_turns"], "true");
        assert_eq!(
            serde_json::from_str::<Vec<String>>(&query["keyterms_prompt"])
                .unwrap()
                .len(),
            100
        );
    }

    #[test]
    fn request_rejects_missing_key_and_non_websocket_endpoints() {
        let mut creds = credentials();
        creds.api_key.clear();
        assert!(matches!(
            creds.request(),
            Err(AssemblyAiError::CredentialsMissing)
        ));
        creds.api_key = "key".into();
        for endpoint in ["https://streaming.assemblyai.com/v3/ws", "not-a-url"] {
            creds.endpoint = endpoint.into();
            assert!(matches!(
                creds.request(),
                Err(AssemblyAiError::InvalidEndpoint)
            ));
        }
    }

    #[tokio::test]
    async fn turn_snapshots_replace_deduplicate_and_sort() {
        let asr = AssemblyAiStreamingASR::new(credentials());
        let sink = Arc::new(super::super::TranscriptCapture::default());
        asr.set_partial_sink(sink.clone());
        asr.handle_response(&turn(0, "hello", false, false));
        asr.handle_response(&turn(0, "hello world", true, false));
        asr.handle_response(&turn(0, "Hello world.", true, true));
        asr.handle_response(&turn(0, "stale", false, false));
        asr.handle_response(&turn(0, "hello world", true, false));
        asr.handle_response(&turn(2, "Third.", true, true));
        asr.handle_response(&turn(1, "Second.", true, true));
        asr.handle_response(&turn(1, "Second.", true, true));
        assert_eq!(transcript(&asr.state.lock()), "Hello world. Second. Third.");
        sink.assert_snapshots(&[
            "hello",
            "hello world",
            "Hello world.",
            "Hello world. Third.",
            "Hello world. Second. Third.",
            "Hello world. Second. Third.",
        ]);
    }

    #[tokio::test]
    async fn termination_returns_empty_transcript_for_silence() {
        let asr = AssemblyAiStreamingASR::new(credentials());
        let (tx, rx) = oneshot::channel();
        asr.state.lock().final_tx = Some(tx);
        asr.consume_pcm_chunk(&vec![0; 3_200]);
        asr.state.lock().termination_sent = true;
        assert!(!asr.handle_response(&json!({"type": "Termination"})));
        let result = rx.await.unwrap().unwrap();
        assert_eq!(result.text, "");
        assert_eq!(result.duration_ms, 100);
        asr.consume_pcm_chunk(&[0; 32]);
        assert_eq!(asr.state.lock().bytes_received, 3_200);
    }

    #[tokio::test]
    async fn provider_error_is_not_hidden_by_partial_text() {
        let asr = AssemblyAiStreamingASR::new(credentials());
        let (tx, rx) = oneshot::channel();
        asr.state.lock().final_tx = Some(tx);
        asr.handle_response(&turn(0, "partial", false, false));
        assert!(!asr.handle_response(&json!({"type": "Error", "error": "invalid API key"})));
        assert!(
            matches!(rx.await.unwrap(), Err(AssemblyAiError::Task(message)) if message == "invalid API key")
        );
    }

    #[tokio::test]
    async fn streams_pcm_then_terminate_and_waits_for_last_turn() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("ws://{}/v3/ws", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_hdr_async(
                socket,
                |request: &tokio_tungstenite::tungstenite::handshake::server::Request, response| {
                    assert_eq!(request.headers()["Authorization"], "test-key");
                    assert!(request.uri().query().unwrap().contains("speech_model="));
                    Ok(response)
                },
            )
            .await
            .unwrap();
            ws.send(Message::Text(
                json!({"type": "Begin", "id": "test"}).to_string().into(),
            ))
            .await
            .unwrap();
            let Message::Binary(first) = ws.next().await.unwrap().unwrap() else {
                panic!("expected PCM");
            };
            assert_eq!(first.len(), AUDIO_CHUNK_BYTES);
            assert!(first.iter().all(|byte| *byte == 1));
            let Message::Binary(tail) = ws.next().await.unwrap().unwrap() else {
                panic!("expected tail");
            };
            assert_eq!(tail.len(), MIN_AUDIO_BYTES);
            assert_eq!(&tail[..320], &[1; 320]);
            assert!(tail[320..].iter().all(|byte| *byte == 0));
            let Message::Text(terminate) = ws.next().await.unwrap().unwrap() else {
                panic!("expected Terminate");
            };
            assert_eq!(
                serde_json::from_str::<Value>(&terminate).unwrap(),
                json!({"type": "Terminate"})
            );
            ws.send(Message::Text(
                turn(0, "Final words.", true, true).to_string().into(),
            ))
            .await
            .unwrap();
            ws.send(Message::Text(
                json!({"type": "Termination"}).to_string().into(),
            ))
            .await
            .unwrap();
            // Client closes after the final server confirmation, not on Terminate.
            assert!(matches!(ws.next().await, Some(Ok(Message::Close(_)))));
        });
        let mut creds = credentials();
        creds.endpoint = endpoint;
        let asr = Arc::new(AssemblyAiStreamingASR::new(creds));
        asr.consume_pcm_chunk(&vec![1; 3_520]);
        asr.open_session().await.unwrap();
        asr.send_last_frame().await.unwrap();
        let result = asr.await_final_result().await.unwrap();
        assert_eq!(result.text, "Final words.");
        assert_eq!(result.duration_ms, 110);
        tokio::time::timeout(Duration::from_secs(2), server)
            .await
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn replay_longer_than_one_write_timeout_drains_before_terminate() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut creds = credentials();
        creds.endpoint = format!("ws://{}/v3/ws", listener.local_addr().unwrap());
        let bytes = 7 * 32_000;
        let server = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(socket).await.unwrap();
            ws.send(Message::Text(json!({"type": "Begin"}).to_string().into())).await.unwrap();
            let mut received = 0;
            while let Some(message) = ws.next().await {
                match message.unwrap() {
                    Message::Binary(audio) => received += audio.len(),
                    Message::Text(text) => {
                        assert_eq!(serde_json::from_str::<Value>(&text).unwrap()["type"], "Terminate");
                        break;
                    }
                    other => panic!("unexpected message {other:?}"),
                }
            }
            assert_eq!(received, bytes);
            ws.send(Message::Text(turn(0, "All audio received.", true, true).to_string().into())).await.unwrap();
            ws.send(Message::Text(json!({"type": "Termination"}).to_string().into())).await.unwrap();
        });
        let asr = Arc::new(AssemblyAiStreamingASR::new(creds));
        asr.open_session().await.unwrap();
        asr.consume_pcm_chunk(&vec![1; bytes]);
        asr.send_last_frame().await.unwrap();
        assert_eq!(asr.await_final_result().await.unwrap().text, "All audio received.");
        server.await.unwrap();
    }

    #[tokio::test]
    async fn cancel_closes_idle_connection() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut creds = credentials();
        creds.endpoint = format!("ws://{}/v3/ws", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(socket).await.unwrap();
            ws.send(Message::Text(json!({"type": "Begin"}).to_string().into()))
                .await
                .unwrap();
            assert!(matches!(ws.next().await, Some(Ok(Message::Close(_)))));
        });
        let asr = Arc::new(AssemblyAiStreamingASR::new(creds));
        asr.open_session().await.unwrap();
        asr.cancel();
        assert!(asr.await_final_result().await.is_err());
        tokio::time::timeout(Duration::from_secs(2), server)
            .await
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn dropping_session_closes_billable_connection() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut creds = credentials();
        creds.endpoint = format!("ws://{}/v3/ws", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(socket).await.unwrap();
            ws.send(Message::Text(json!({"type": "Begin"}).to_string().into()))
                .await
                .unwrap();
            assert!(matches!(ws.next().await, Some(Ok(Message::Close(_)))));
        });
        let asr = Arc::new(AssemblyAiStreamingASR::new(creds));
        asr.open_session().await.unwrap();
        drop(asr);
        tokio::time::timeout(Duration::from_secs(2), server)
            .await
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn error_before_begin_fails_open_and_closes_connection() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut creds = credentials();
        creds.endpoint = format!("ws://{}/v3/ws", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(socket).await.unwrap();
            ws.send(Message::Text(
                json!({"type": "Error", "error": "quota exceeded"})
                    .to_string()
                    .into(),
            ))
            .await
            .unwrap();
            assert!(matches!(ws.next().await, Some(Ok(Message::Close(_)))));
        });
        let asr = Arc::new(AssemblyAiStreamingASR::new(creds));
        assert!(
            matches!(asr.open_session().await, Err(AssemblyAiError::Task(message)) if message.contains("quota exceeded"))
        );
        tokio::time::timeout(Duration::from_secs(2), server)
            .await
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn abrupt_disconnect_is_an_error_not_a_successful_partial() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut creds = credentials();
        creds.endpoint = format!("ws://{}/v3/ws", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(socket).await.unwrap();
            ws.send(Message::Text(json!({"type": "Begin"}).to_string().into()))
                .await
                .unwrap();
            ws.send(Message::Text(
                turn(0, "unfinished", false, false).to_string().into(),
            ))
            .await
            .unwrap();
            ws.close(None).await.unwrap();
        });
        let asr = Arc::new(AssemblyAiStreamingASR::new(creds));
        asr.open_session().await.unwrap();
        assert!(matches!(
            asr.await_final_result().await,
            Err(AssemblyAiError::Closed)
        ));
        server.await.unwrap();
    }
}
