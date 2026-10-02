//! Soniox real-time Speech-to-Text client.
//!
//! Uses the Soniox real-time WebSocket protocol
//! (`wss://stt-rt.soniox.com/transcribe-websocket`): the client sends a single
//! JSON config message carrying the API key + audio format, then a stream of
//! binary 16 kHz / 16-bit / mono PCM chunks, then an empty text frame (`""`)
//! to signal end-of-audio. The server streams back JSON responses describing
//! an evolving token list (`tokens[].{text, is_final}`) until `finished == true`.
//!
//! Structurally mirrors `bailian.rs` (DashScope realtime WebSocket) so it slots
//! into the same session / `DeferredAsrBridge` / credential infrastructure.
//!
//! Key protocol difference from DashScope: Soniox streams a *linear* token list
//! per response rather than sentence ids. Final tokens (`is_final == true`) are
//! guaranteed by the spec to be emitted exactly once and never repeated, so a
//! single `String` accumulator keyed only on final tokens is correct here — no
//! need for the `BTreeMap<sentence_id, _>` dedup used in `bailian.rs`.

use std::sync::Arc;
use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use parking_lot::Mutex as ParkingMutex;
use serde_json::{json, Value};
use tokio::net::TcpStream;
use tokio::runtime::Handle;
use tokio::sync::{mpsc, oneshot, Mutex as AsyncMutex};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};

use super::{AudioConsumer, RawTranscript};

pub const PROVIDER_ID: &str = "soniox";
pub const DEFAULT_ENDPOINT: &str = "wss://stt-rt.soniox.com/transcribe-websocket";
pub const DEFAULT_MODEL: &str = "stt-rt-v5";

/// 100 ms of 16 kHz / 16-bit / mono PCM.
pub const TARGET_AUDIO_CHUNK_BYTES: usize = 3_200;
const BYTES_PER_MS: u64 = 32;
const FINAL_RESULT_TIMEOUT: Duration = Duration::from_secs(12);

type WsStream = WebSocketStream<MaybeTlsStream<TcpStream>>;
type WsSink = futures_util::stream::SplitSink<WsStream, Message>;
type SharedWriter = Arc<AsyncMutex<Option<WsSink>>>;

#[derive(Clone, Debug)]
pub struct SonioxCredentials {
    pub api_key: String,
    pub endpoint: String,
    pub model: String,
    /// 用户词典启用短语，映射为 Soniox `context.terms`（领域/专有名词偏置）。
    pub terms: Vec<String>,
}

impl SonioxCredentials {
    pub fn normalized_endpoint(&self) -> String {
        if self.endpoint.trim().is_empty() {
            return DEFAULT_ENDPOINT.to_string();
        }
        self.endpoint.trim().to_string()
    }

    pub fn normalized_model(&self) -> String {
        let model = self.model.trim();
        if model.is_empty() {
            DEFAULT_MODEL.to_string()
        } else {
            model.to_string()
        }
    }
}

/// Soniox 实时 ASR 走 WebSocket 网关，接口地址只接受 ws:// 或 wss://。
/// 用户容易把控制台 https:// 地址粘进来——那是另一套 HTTP 协议，WebSocket
/// 握手必然失败且底层报错对用户不可读。在验证入口先拦下，前端据
/// `sonioxEndpointSchemeInvalid` 错误码给出可操作提示（与 `bailian` 同形）。
pub fn endpoint_scheme_is_websocket(endpoint: &str) -> bool {
    let lower = endpoint.trim().to_ascii_lowercase();
    lower.starts_with("wss://") || lower.starts_with("ws://")
}

#[derive(Debug, thiserror::Error)]
pub enum SonioxASRError {
    #[error("credentials missing")]
    CredentialsMissing,
    #[error("connection failed: {0}")]
    ConnectionFailed(String),
    #[error("send failed: {0}")]
    SendFailed(String),
    #[error("task failed: {0}")]
    TaskFailed(String),
    #[error("no final result")]
    NoFinalResult,
    #[error("final result timed out")]
    FinalResultTimeout,
}

enum SendItem {
    Audio(Vec<u8>),
    Finish(oneshot::Sender<Result<(), SonioxASRError>>),
}

#[derive(Default)]
struct SyncState {
    pending_audio: Vec<u8>,
    audio_scratch: Vec<u8>,
    bytes_received: u64,
    task_started: bool,
    task_finished: bool,
    runtime: Option<Handle>,
    start: Option<Instant>,
    final_tx: Option<oneshot::Sender<Result<RawTranscript, SonioxASRError>>>,
    send_tx: Option<mpsc::UnboundedSender<SendItem>>,
    /// 已完成的 final token 累积文本。Soniox 保证 final token 只发一次且不变，
    /// 因此直接追加即可，无需 sentence_id 去重（与 `bailian.rs` 的 BTreeMap 方案有意 diverge）。
    final_text: String,
    config_sent: bool,
}

pub struct SonioxStreamingASR {
    credentials: SonioxCredentials,
    state: ParkingMutex<SyncState>,
    writer: SharedWriter,
    final_rx: ParkingMutex<Option<oneshot::Receiver<Result<RawTranscript, SonioxASRError>>>>,
}

impl SonioxStreamingASR {
    pub fn new(credentials: SonioxCredentials) -> Self {
        Self {
            credentials,
            state: ParkingMutex::new(SyncState::default()),
            writer: Arc::new(AsyncMutex::new(None)),
            final_rx: ParkingMutex::new(None),
        }
    }

    pub async fn open_session(self: &Arc<Self>) -> Result<(), SonioxASRError> {
        if self.credentials.api_key.trim().is_empty() {
            return Err(SonioxASRError::CredentialsMissing);
        }

        let endpoint = self.credentials.normalized_endpoint();
        let request = endpoint
            .into_client_request()
            .map_err(|e| SonioxASRError::ConnectionFailed(e.to_string()))?;

        let (ws, _resp) = connect_async(request)
            .await
            .map_err(|e| SonioxASRError::ConnectionFailed(e.to_string()))?;
        let (write, read) = ws.split();
        *self.writer.lock().await = Some(write);

        let (final_tx, final_rx) = oneshot::channel();
        let (send_tx, mut send_rx) = mpsc::unbounded_channel::<SendItem>();
        {
            let mut st = self.state.lock();
            *st = SyncState::default();
            st.runtime = Some(Handle::current());
            st.start = Some(Instant::now());
            st.final_tx = Some(final_tx);
            st.send_tx = Some(send_tx);
        }
        *self.final_rx.lock() = Some(final_rx);

        let writer_for_worker = Arc::clone(&self.writer);
        tokio::spawn(async move {
            while let Some(item) = send_rx.recv().await {
                match item {
                    SendItem::Audio(chunk) => {
                        if let Err(e) = send_binary(&writer_for_worker, chunk).await {
                            log::error!("[soniox-asr] audio frame send failed: {e}");
                        }
                    }
                    SendItem::Finish(done) => {
                        let result =
                            send_text(&writer_for_worker, String::new()).await.map_err(|e| {
                                SonioxASRError::SendFailed(e.to_string())
                            });
                        let _ = done.send(result);
                    }
                }
            }
        });

        // 配置消息必须在任何音频之前发出。
        send_text(
            &self.writer,
            build_config_message(
                self.credentials.api_key.trim(),
                &self.credentials.normalized_model(),
                &self.credentials.terms,
            ),
        )
        .await?;
        self.state.lock().config_sent = true;

        let weak_self = Arc::downgrade(self);
        tokio::spawn(async move {
            let mut read = read;
            while let Some(msg) = read.next().await {
                let Some(this) = weak_self.upgrade() else {
                    break;
                };
                match msg {
                    Ok(Message::Text(text)) => {
                        if !this.handle_text_message(&text) {
                            break;
                        }
                    }
                    Ok(Message::Close(_)) => {
                        this.finish_with_partial_or_error(SonioxASRError::NoFinalResult);
                        break;
                    }
                    Ok(_) => {}
                    Err(e) => {
                        log::error!("[soniox-asr] receive loop error: {e}");
                        this.finish_with_partial_or_error(SonioxASRError::ConnectionFailed(
                            e.to_string(),
                        ));
                        break;
                    }
                }
            }
        });

        Ok(())
    }

    pub async fn send_last_frame(&self) -> Result<(), SonioxASRError> {
        // Soniox 没有 task-started 回执：config_sent 一置位音频即可送达，消费者
        // 也据 config_sent 放行（见 consume_pcm_chunk）。这里只把残留 audio_scratch
        // 一次性 flush 再发空字符串结束信号。
        let (send_tx, tail_chunks) = {
            let mut st = self.state.lock();
            let send_tx = st.send_tx.clone();
            if !st.pending_audio.is_empty() {
                let pending = std::mem::take(&mut st.pending_audio);
                st.audio_scratch.extend_from_slice(&pending);
            }
            let tail = if st.audio_scratch.is_empty() {
                Vec::new()
            } else {
                vec![std::mem::take(&mut st.audio_scratch)]
            };
            (send_tx, tail)
        };
        if let Some(send_tx) = send_tx {
            for chunk in tail_chunks {
                let _ = send_tx.send(SendItem::Audio(chunk));
            }
            let (done_tx, done_rx) = oneshot::channel();
            send_tx
                .send(SendItem::Finish(done_tx))
                .map_err(|_| SonioxASRError::SendFailed("send worker closed".to_string()))?;
            done_rx
                .await
                .map_err(|_| SonioxASRError::SendFailed("finish ack dropped".to_string()))??
        }
        Ok(())
    }

    pub async fn await_final_result(&self) -> Result<RawTranscript, SonioxASRError> {
        let rx = self.final_rx.lock().take();
        let Some(rx) = rx else {
            return Err(SonioxASRError::NoFinalResult);
        };
        tokio::time::timeout(FINAL_RESULT_TIMEOUT, rx)
            .await
            .map_err(|_| SonioxASRError::FinalResultTimeout)?
            .map_err(|_| SonioxASRError::NoFinalResult)?
    }

    pub fn cancel(&self) {
        let mut st = self.state.lock();
        st.pending_audio.clear();
        st.audio_scratch.clear();
        st.send_tx.take();
        st.final_tx.take();
        st.task_finished = true;
        drop(st);
        let writer = Arc::clone(&self.writer);
        if let Ok(handle) = Handle::try_current() {
            handle.spawn(async move {
                let _ = close_writer(&writer).await;
            });
        } else {
            std::thread::spawn(move || {
                if let Ok(rt) = tokio::runtime::Runtime::new() {
                    rt.block_on(async move {
                        let _ = close_writer(&writer).await;
                    });
                }
            });
        }
    }

    fn handle_text_message(&self, text: &str) -> bool {
        let value: Value = match serde_json::from_str(text) {
            Ok(v) => v,
            Err(e) => {
                log::warn!("[soniox-asr] invalid json event: {e}");
                return true;
            }
        };
        self.handle_response(&value)
    }

    /// 解码一条 Soniox 响应，推进 `state`。
    ///
    /// 返回 `false` 表示会话应当终止（`finished` 或致命错误），由 read loop
    /// 据此跳出；返回 `true` 表示继续接收。
    ///
    /// Soniox 响应字段：
    /// - `tokens: [{text, is_final, speaker?, language?}]`
    /// - `finished: bool`
    /// - `error_code: string | null`
    /// - `error_message: string | null`
    fn handle_response(&self, value: &Value) -> bool {
        // 错误优先：error_code 出现即终止并回报（与 Soniox 文档约定一致）。
        if let Some(code) = value.get("error_code").and_then(Value::as_str) {
            if !code.is_empty() {
                let message = value
                    .get("error_message")
                    .and_then(Value::as_str)
                    .unwrap_or("soniox task failed")
                    .to_string();
                self.mark_task_started();
                self.finish_error(SonioxASRError::TaskFailed(message));
                return false;
            }
        }

        // 追加 final token 文本。非 final token 会随后续响应变化/被替换，
        // 不能累积，否则会重复（与 Soniox 文档「final tokens are sent only
        // once and never repeated」一致）。
        if let Some(tokens) = value.get("tokens").and_then(Value::as_array) {
            for token in tokens {
                let is_final = token.get("is_final").and_then(Value::as_bool).unwrap_or(false);
                if !is_final {
                    continue;
                }
                if let Some(text) = token.get("text").and_then(Value::as_str) {
                    if !text.is_empty() {
                        self.state.lock().final_text.push_str(text);
                    }
                }
            }
        }

        self.mark_task_started();

        if value.get("finished").and_then(Value::as_bool).unwrap_or(false) {
            self.finish_success();
            false
        } else {
            true
        }
    }

    fn mark_task_started(&self) {
        // Soniox 没有 task-started 回执：config 一发出音频即可送达（consume_pcm_chunk
        // 据 config_sent 放行）。首次响应到来时把 open_session 之后可能暂存的
        // pending_audio 一次性 flush，作为 config_sent 置位竞态的稳妥兜底。
        let mut st = self.state.lock();
        if st.task_started {
            return;
        }
        st.task_started = true;
        if !st.pending_audio.is_empty() && st.config_sent {
            let pending = std::mem::take(&mut st.pending_audio);
            st.audio_scratch.extend_from_slice(&pending);
            let send_tx = st.send_tx.clone();
            let chunks = drain_audio_chunks(&mut st.audio_scratch);
            if let Some(tx) = send_tx {
                for chunk in chunks {
                    let _ = tx.send(SendItem::Audio(chunk));
                }
            }
        }
    }

    fn finish_success(&self) {
        let (tx, text, duration_ms) = {
            let mut st = self.state.lock();
            if st.task_finished {
                return;
            }
            st.task_finished = true;
            st.send_tx.take();
            let text = std::mem::take(&mut st.final_text);
            let duration_ms = if st.bytes_received > 0 {
                st.bytes_received / BYTES_PER_MS
            } else {
                st.start
                    .map(|start| start.elapsed().as_millis() as u64)
                    .unwrap_or_default()
            };
            (st.final_tx.take(), text, duration_ms)
        };
        if let Some(tx) = tx {
            let _ = tx.send(Ok(RawTranscript { text, duration_ms }));
        }
        self.close_on_runtime();
    }

    fn finish_with_partial_or_error(&self, error: SonioxASRError) {
        let has_partial = {
            let st = self.state.lock();
            !st.final_text.trim().is_empty()
        };
        if has_partial {
            // 与 Volcengine / Bailian 一致：连接异常但已有 partial 时优先兜底返回，避免丢失已识别内容。
            self.finish_success();
        } else {
            self.finish_error(error);
        }
    }

    fn finish_error(&self, error: SonioxASRError) {
        let tx = {
            let mut st = self.state.lock();
            if st.task_finished {
                return;
            }
            st.task_finished = true;
            st.send_tx.take();
            st.final_tx.take()
        };
        if let Some(tx) = tx {
            let _ = tx.send(Err(error));
        }
        self.close_on_runtime();
    }

    fn close_on_runtime(&self) {
        let writer = Arc::clone(&self.writer);
        if let Some(handle) = self.state.lock().runtime.clone() {
            handle.spawn(async move {
                let _ = close_writer(&writer).await;
            });
        }
    }
}

impl AudioConsumer for SonioxStreamingASR {
    fn consume_pcm_chunk(&self, pcm: &[u8]) {
        if pcm.is_empty() {
            return;
        }
        let (send_tx, chunks) = {
            let mut st = self.state.lock();
            st.bytes_received = st.bytes_received.saturating_add(pcm.len() as u64);
            if !st.config_sent {
                st.pending_audio.extend_from_slice(pcm);
                return;
            }
            st.audio_scratch.extend_from_slice(pcm);
            let chunks = drain_audio_chunks(&mut st.audio_scratch);
            (st.send_tx.clone(), chunks)
        };
        if let Some(tx) = send_tx {
            for chunk in chunks {
                let _ = tx.send(SendItem::Audio(chunk));
            }
        }
    }
}

fn drain_audio_chunks(buffer: &mut Vec<u8>) -> Vec<Vec<u8>> {
    let mut chunks = Vec::new();
    while buffer.len() >= TARGET_AUDIO_CHUNK_BYTES {
        chunks.push(buffer.drain(..TARGET_AUDIO_CHUNK_BYTES).collect());
    }
    chunks
}

/// 构造 Soniox 初始配置消息。
///
/// - `audio_format: "pcm_s16le"` + `sample_rate: 16000` + `num_channels: 1`
///   直接匹配 OpenLess recorder 的原始输出，无需转码容器。
/// - `enable_endpoint_detection: true`：说话人停顿时让服务端 finalize 当前
///   词汇，使最终文本更干净（与 app 其它流式 provider 的收尾语义对齐）。
/// - `context.terms`：把用户词典启用的短语作为领域/专有名词偏置喂给模型。
pub fn build_config_message(api_key: &str, model: &str, terms: &[String]) -> String {
    let mut config = json!({
        "api_key": api_key,
        "model": model,
        "audio_format": "pcm_s16le",
        "sample_rate": 16_000,
        "num_channels": 1,
        "enable_endpoint_detection": true,
    });

    let terms: Vec<&str> = terms
        .iter()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .collect();
    if !terms.is_empty() {
        config["context"] = json!({ "terms": terms });
    }

    config.to_string()
}

async fn send_text(writer: &SharedWriter, text: String) -> Result<(), SonioxASRError> {
    let mut guard = writer.lock().await;
    let Some(ws) = guard.as_mut() else {
        return Err(SonioxASRError::ConnectionFailed(
            "websocket writer not available".to_string(),
        ));
    };
    ws.send(Message::Text(text))
        .await
        .map_err(|e| SonioxASRError::SendFailed(e.to_string()))
}

async fn send_binary(writer: &SharedWriter, data: Vec<u8>) -> Result<(), SonioxASRError> {
    let mut guard = writer.lock().await;
    let Some(ws) = guard.as_mut() else {
        return Err(SonioxASRError::ConnectionFailed(
            "websocket writer not available".to_string(),
        ));
    };
    ws.send(Message::Binary(data))
        .await
        .map_err(|e| SonioxASRError::SendFailed(e.to_string()))
}

async fn close_writer(writer: &SharedWriter) -> Result<(), SonioxASRError> {
    let mut guard = writer.lock().await;
    if let Some(mut ws) = guard.take() {
        let _ = ws.close().await;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn creds() -> SonioxCredentials {
        SonioxCredentials {
            api_key: "sk-test".to_string(),
            endpoint: String::new(),
            model: String::new(),
            terms: Vec::new(),
        }
    }

    fn make_asr() -> SonioxStreamingASR {
        SonioxStreamingASR::new(creds())
    }

    fn make_response(tokens: &[(&str, bool)], finished: bool) -> Value {
        let tokens: Vec<Value> = tokens
            .iter()
            .map(|(text, is_final)| {
                json!({ "text": text, "is_final": is_final })
            })
            .collect();
        json!({ "tokens": tokens, "finished": finished })
    }

    fn tokens_of(asr: &SonioxStreamingASR) -> String {
        asr.state.lock().final_text.clone()
    }

    fn is_finished(asr: &SonioxStreamingASR) -> bool {
        asr.state.lock().task_finished
    }

    /// 安装一个 oneshot 终结 channel，让 `finish_success`/`finish_error` 的产出
    /// 能被取回（`open_session` 真实路径会装一个；单测里不走网络，直接手动装一个）。
    /// 返回 receiver 供断言 await。
    fn install_final_tx(asr: &SonioxStreamingASR) -> oneshot::Receiver<Result<RawTranscript, SonioxASRError>> {
        let (tx, rx) = oneshot::channel();
        asr.state.lock().final_tx = Some(tx);
        rx
    }

    #[test]
    fn config_message_has_required_fields() {
        let msg = build_config_message("sk-test", "stt-rt-v5", &[]);
        let v: Value = serde_json::from_str(&msg).unwrap();
        assert_eq!(v["api_key"], "sk-test");
        assert_eq!(v["model"], "stt-rt-v5");
        assert_eq!(v["audio_format"], "pcm_s16le");
        assert_eq!(v["sample_rate"], 16_000);
        assert_eq!(v["num_channels"], 1);
        assert_eq!(v["enable_endpoint_detection"], true);
        assert!(v.get("context").is_none(), "no terms -> no context block");
    }

    #[test]
    fn config_message_includes_terms_when_present() {
        let terms = vec!["Celebrex".to_string(), "  ".to_string(), "Zyrtec".to_string()];
        let msg = build_config_message("k", "stt-rt-v5", &terms);
        let v: Value = serde_json::from_str(&msg).unwrap();
        let context_terms = v["context"]["terms"].as_array().unwrap();
        // 空白项被过滤
        assert_eq!(context_terms.len(), 2);
        assert_eq!(context_terms[0], "Celebrex");
        assert_eq!(context_terms[1], "Zyrtec");
    }

    #[tokio::test]
    async fn handle_response_accumulates_only_final_tokens() {
        let asr = make_asr();
        let rx = install_final_tx(&asr);

        let resp = make_response(&[("How", false), ("'re", false)], false);
        asr.handle_response(&resp);
        assert_eq!(tokens_of(&asr), "", "non-final tokens must not accumulate");

        let resp = make_response(&[("How", true), (" ", true), ("are", false)], false);
        asr.handle_response(&resp);
        assert_eq!(tokens_of(&asr), "How ");

        // finished 响应到达后 finish_success 会 take final_text 并通过 oneshot
        // 交付最终转写；这里从 oneshot receiver 取回并对它断言，而不是读已被
        // take 清空的 final_text。
        let resp = make_response(&[("you", true), ("?", true)], true);
        asr.handle_response(&resp);
        assert!(is_finished(&asr));
        let transcript = rx.await.unwrap().unwrap();
        assert_eq!(transcript.text, "How you?");
    }

    #[tokio::test]
    async fn handle_response_delivers_full_transcript_on_finish() {
        // 与上面互补：验收最终文本走 oneshot 交付路径，对齐真实使用。
        let asr = make_asr();
        let rx = install_final_tx(&asr);

        let r1 = make_response(&[("How", false)], false);
        asr.handle_response(&r1);
        let r2 = make_response(&[("How", true), (" ", true), ("are", true), (" you", true)], false);
        asr.handle_response(&r2);
        let r3 = make_response(&[("?", true)], true);
        asr.handle_response(&r3);

        let transcript = rx.await.unwrap().unwrap();
        assert_eq!(transcript.text, "How are you?");
    }

    #[test]
    fn handle_response_handles_error_code() {
        let asr = make_asr();
        let err = json!({
            "error_code": "invalidApiKey",
            "error_message": "API key is invalid",
            "tokens": [],
            "finished": true,
        });
        asr.handle_response(&err);
        assert!(is_finished(&asr), "error code must finish session");
        assert!(tokens_of(&asr).is_empty());
    }

    #[tokio::test]
    async fn handle_response_partial_then_final_preserves_order() {
        // 模拟逐字演化：先非 final 的 "How"，再 final 的 "How", final 的 " are"，
        // 非 final 的 "you"；最终 final 的 "you"。只应累积 final 项且顺序保留。
        let asr = make_asr();
        let rx = install_final_tx(&asr);

        let r1 = make_response(&[("How", false)], false);
        asr.handle_response(&r1);
        assert_eq!(tokens_of(&asr), "");

        let r2 = make_response(&[("How", true), (" are", true)], false);
        asr.handle_response(&r2);
        assert_eq!(tokens_of(&asr), "How are");

        let r3 = make_response(&[("you", false), ("?", false)], false);
        asr.handle_response(&r3);
        assert_eq!(tokens_of(&asr), "How are", "non-final must not duplicate");

        let r4 = make_response(&[("you", true), ("?", true)], true);
        asr.handle_response(&r4);
        // finished 后 final_text 被 take 并通过 oneshot 交付，最终文本走
        // oneshot receiver（与真实 await_final_result 路径一致）。
        let transcript = rx.await.unwrap().unwrap();
        assert_eq!(transcript.text, "How areyou?");
    }

    #[test]
    fn empty_key_rejected_before_connection() {
        let creds = SonioxCredentials {
            api_key: "  ".to_string(),
            endpoint: DEFAULT_ENDPOINT.to_string(),
            model: DEFAULT_MODEL.to_string(),
            terms: Vec::new(),
        };
        let asr = std::sync::Arc::new(SonioxStreamingASR::new(creds));
        // open_session 需要 tokio runtime；这里只断言 api_key 校验路径，
        // 不真正握手。
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let err = rt
            .block_on(async move { asr.open_session().await })
            .unwrap_err();
        assert!(matches!(err, SonioxASRError::CredentialsMissing));
    }

    #[test]
    fn endpoint_scheme_validation() {
        assert!(endpoint_scheme_is_websocket("wss://stt-rt.soniox.com/x"));
        assert!(endpoint_scheme_is_websocket("ws://localhost:9000/x"));
        assert!(!endpoint_scheme_is_websocket("https://stt-rt.soniox.com/x"));
        assert!(!endpoint_scheme_is_websocket("stt-rt.soniox.com"));
    }

    #[test]
    fn normalized_endpoint_falls_back_to_default() {
        let c = SonioxCredentials {
            api_key: "k".into(),
            endpoint: "  ".into(),
            model: "  ".into(),
            terms: Vec::new(),
        };
        assert_eq!(c.normalized_endpoint(), DEFAULT_ENDPOINT);
        assert_eq!(c.normalized_model(), DEFAULT_MODEL);
    }
}