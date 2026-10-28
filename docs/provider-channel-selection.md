# Provider channel selection

For the separate ASR and LLM channel lists, the first enabled channel is the
selected channel. Order is ascending, with channel ID breaking equal-order ties.
The settings list, credential status and new requests must follow this rule.

The stored active channel ID is a compatibility cache, not an independent
selection. Loading desktop credentials reconciles it with the channel list,
including already migrated vaults and restored data. Channel mutations update
the cache. Reconciliation changes the loaded copy; the next authorized credential
mutation persists it through the existing storage path.

New requests resolve both channel ID and provider type from the same channel
list snapshot, then look up that channel's model and credentials. A provider type
such as `local-qwen3-mlx` must not bypass configured channels or their enabled
state. A nonempty list with every channel disabled has no selected channel and
must not fall back to an old preference. Legacy active values and preferences
remain supported when there is no channel configuration. Omni keeps its own
explicit selection.

The local model page reads credential status and listens for credential changes
to display the current provider. Local model preferences still remember the
model configuration. Switching away from a local channel does not delete its
downloaded model or credentials; enable it and move it first to use it again.

Restart reconciliation checks local selection metadata. It does not send a
provider validation request or automatically choose another enabled provider
after a network failure. An ongoing recording retains the channel identity,
protocol and model selected at the start of that recording.

## AssemblyAI streaming speech recognition

Add an **AssemblyAI realtime ASR** channel in the ASR provider settings, enter
your AssemblyAI API key, then enable the channel and move it first. The default
model is `universal-3-6-pro`; the model picker also offers `universal-3-5-pro`,
`universal-streaming-english`, and `universal-streaming-multilingual`.

The default endpoint is `wss://streaming.assemblyai.com/v3/ws`. For data residency,
use `wss://streaming.us.assemblyai.com/v3/ws` or
`wss://streaming.eu.assemblyai.com/v3/ws`. Authentication uses the WebSocket
`Authorization` header, without a `Bearer` prefix; do not put API keys in the URL.

OpenLess sends 16 kHz mono PCM16 audio and displays full transcript snapshots as
turns arrive. Enabled dictionary phrases become `keyterms_prompt`, with duplicate
and empty phrases removed; only the first 100 unique phrases of at most 50
characters each are sent. A formatted turn replaces its earlier text rather than
being appended again.

Stopping recording sends `Terminate` and waits for the last turn and `Termination`
before closing. Cancellation and timeouts close the connection as well, since
AssemblyAI bills streaming sessions for connection time. **Test connection** also
opens a short billable streaming session using silence.

Protocol reference: [AssemblyAI streaming WebSocket API](https://www.assemblyai.com/docs/streaming/api-spec/streaming-websocket).


## Use your ChatGPT plan for text polishing

Add a **ChatGPT Plan** LLM channel and choose **Continue with ChatGPT**. Complete
sign-in and plan-usage consent in the system browser, return to OpenLess, and
select a model from the account's catalog. Enable the channel and move it first
to use it for polishing and translation. Codex and an OpenAI API key are not
required. Speech recognition keeps its existing ASR provider.

Model discovery does not run inference. **Test connection** consumes a small
amount of ChatGPT plan usage. **Manage usage** opens ChatGPT's usage settings;
limits may apply to the plan or this app. OpenLess never switches a failed
ChatGPT Plan request to an API-key billing path automatically.

Each channel selects a saved account registration or creates a new one. Matching
email addresses do not merge registrations or workspaces. **Disconnect** stops
that registration's requests, attempts remote session revocation, and removes
its local tokens without changing channel order. If remote revocation cannot be
confirmed, disconnect the app in ChatGPT settings. Reauthorization reuses the
saved registration. OAuth tokens and host identifiers are local-only and are
excluded from provider synchronization, backup, and migration; a channel copied
to another device requires a new local sign-in.

On mobile, keep the authorization within the system's background-execution
window. If the process is terminated or the attempt expires, start sign-in again.
