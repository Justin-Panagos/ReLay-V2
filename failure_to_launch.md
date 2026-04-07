# Failure to Launch

Critical issues that must be resolved before ReLay can ship. Ordered by severity — items at
the top will cause the most immediate damage to users or the business model. Each item includes
the root cause, the recommended fix, and the file(s) involved.

---

## 1. ~~Payment Pipeline — HMAC Signature Bypass~~ ✓ RESOLVED

**Resolved by Paystack migration.**  
Paystack's HMAC-SHA512 verification in `verifyAndParsePaystackEvent()` computes the signature
over the raw body and compares it as a plain hex string — no complex header parsing, so the
original `indexOf('=')` bug is gone. The check throws on a missing or mismatched header.

**Remaining note:** The comparison `computed !== sigHeader` is not constant-time. A timing
attack is theoretically possible but impractical to exploit over a network. Low priority.

---

## 2. ~~Payment Pipeline — No Idempotency on Webhook Events~~ ✓ RESOLVED

**Resolved by Paystack migration.**  
Idempotency is now implemented using `event.id` as the KV key with a 7-day TTL, immediately
after signature verification and before any processing. Paystack retries are handled correctly.

---

## 3. Pro Licence — Settings Flag Trivially Bypassed

**Severity:** Critical — business model  
**File:** `src-tauri/src/pro/mod.rs`

**Problem:**  
`is_pro()` reads `licence_status` from SQLite. Any user can open their database with DB Browser
for SQLite and set that value to `"pro"`. There is no cryptographic verification. Pro features
are not protected.

**Fix:**  
On startup and on a periodic timer (e.g. every 24 hours), re-verify licence status against the
ICP Identity Canister via `check_licence(device_id)`. Cache the result in memory (not only in
the DB) and use the in-memory value for gate checks during the session. Write to the DB only
after a successful canister response so the cached value survives restarts on ICP network
outages. The DB value becomes a fallback, not the source of truth.

```rust
// src-tauri/src/pro/mod.rs
pub fn require_pro(state: &LicenceState) -> Result<(), String> {
    // Read from in-memory state set by ICP verification, not from SQLite directly
    match state.status.lock().unwrap().as_deref() {
        Some("pro") => Ok(()),
        _ => Err("Pro subscription required".into()),
    }
}
```

---

## 4. App Startup — Config Panic Instead of Graceful Error

**Severity:** Critical — all users  
**File:** `src-tauri/src/icp/config.rs`

**Problem:**  
`load_config()` calls `panic!()` if `config.toml` is missing or malformed. Every new install
that hasn't set up the config file gets a hard crash with no user-facing message — just a
process exit.

**Fix:**  
Return `Result<AppConfig, String>` and handle the error in `main.rs` with a Tauri dialog before
the window opens. This turns a silent crash into an actionable message.

```rust
// src-tauri/src/icp/config.rs
pub fn load_config(data_dir: &Path) -> Result<AppConfig, String> {
    let path = data_dir.join("config.toml")
    let contents = std::fs::read_to_string(&path)
        .map_err(|_| format!("config.toml not found at {}. Copy config.toml.example from the repo root and fill in your values.", path.display()))?;
    toml::from_str(&contents)
        .map_err(|e| format!("config.toml is invalid: {e}"))
}

// src-tauri/src/main.rs — in setup
match load_config(&data_dir) {
    Ok(cfg) => { /* proceed */ }
    Err(msg) => {
        tauri::api::dialog::blocking::message(None::<&tauri::Window>, "Configuration Error", msg);
        std::process::exit(1);
    }
}
```

---

## 5. Torrent Controls — Pause / Cancel / Restart Non-Functional

**Severity:** Critical — core feature  
**Files:** `src/scripts/torrent.js`, `src-tauri/src/commands/mod.rs`, `src-tauri/src/torrent/mod.rs`

**Problem:**  
The frontend calls `invoke('pause_torrent', ...)`, `invoke('cancel_torrent', ...)`, and
`invoke('restart_torrent', ...)` but the buttons have no visible effect. The failure point is
somewhere in the chain: frontend invoke → Tauri command handler → librqbit Session API. The
backlog confirms this is known and unresolved.

**Fix approach:**  
Work through the chain in order:
1. Add a `console.log` before each invoke to confirm the call fires
2. Add a `println!` at the top of each Tauri command handler to confirm it receives the call
3. Check the librqbit `Session` handle is still valid at the point of the call — it may have
   been dropped or not stored in state correctly
4. Check the return value from librqbit and propagate errors back to the frontend instead of
   swallowing them in a `catch { /* no-op */ }` block

The `catch {}` blocks in `torrent.js` lines 61, 69, 74 are hiding the actual error. Replace
them with error logging and a toast notification so the failure is visible.

---

## 6. Event Subscription Cleanup — Crash on Failed Listen

**Severity:** Critical — download reliability  
**File:** `src/scripts/main.js`

**Problem:**  
`subscribeToDownloadEvents(id)` calls multiple `await listen(...)` calls in sequence. If any
one fails, the variables for subsequent listeners are never assigned. When `cleanup()` is
eventually called, it tries to invoke those undefined variables and throws
`TypeError: unlistenX is not a function`. The download card then becomes a dead widget — the
backend continues downloading but the UI receives no events.

**Fix:**  
Initialise all unlisten variables as no-ops before the awaits. Each successful listen
overwrites the no-op. Cleanup then always calls a function, never undefined.

```javascript
// src/scripts/main.js — subscribeToDownloadEvents()
async function subscribeToDownloadEvents(id) {
  let unlistenProgress = () => {}
  let unlistenComplete = () => {}
  let unlistenError = () => {}
  let unlistenPaused = () => {}
  let unlistenScan = () => {}
  let unlistenQuarantine = () => {}

  try {
    unlistenProgress = await listen(`download://progress/${id}`, ...)
    unlistenComplete = await listen(`download://complete/${id}`, ...)
    // ... rest of listens
  } catch (err) {
    console.error(`Failed to subscribe to download events for ${id}:`, err)
    // partial subscriptions are still cleaned up correctly below
  }

  return function cleanup() {
    unlistenProgress()
    unlistenComplete()
    unlistenError()
    unlistenPaused()
    unlistenScan()
    unlistenQuarantine()
  }
}
```

---

## 7. Silent Failures — No User Feedback on Action Errors

**Severity:** High — user experience  
**Files:** `src/scripts/main.js`, `src/scripts/torrent.js`, `src/scripts/quarantine.js`, `src/scripts/community.js`, `src/scripts/settings.js`

**Problem:**  
Almost every `catch` block across the frontend either logs to console or is empty. Users who
trigger an action that fails see nothing — no toast, no dialog, no indication that anything
went wrong. Known affected actions:
- Resume / pause / cancel download
- Pause / resume / cancel torrent
- Open file / open folder (file moved or deleted)
- Submit zero-day report
- Cast a governance vote
- Open upgrade page

**Fix:**  
Add a single shared toast utility and call it from every catch block. One function, called
everywhere — not a per-component solution.

```javascript
// src/scripts/main.js (or a shared utils module)
function showErrorToast(message) {
  const toast = document.createElement('div')
  toast.className = 'toast toast-error'
  toast.textContent = message
  document.body.appendChild(toast)
  setTimeout(() => toast.remove(), 4000)
}

// Then in every catch:
catch (err) {
  showErrorToast(`Failed to pause download: ${err}`)
  btn.disabled = false
}
```

The CSS for `.toast` / `.toast-error` already belongs in `styles/components.css` per the
project's CSS architecture rule.

---

## 8. ICP Network Failures — Silent Degradation

**Severity:** High — user trust  
**File:** `src-tauri/src/icp/sync.rs`

**Problem:**  
If the ICP network is unreachable at startup, `sync.rs` silently returns. The user's licence
status is never verified, the pattern database is never updated, and Shield runs on stale data.
The user has no idea any of this is happening.

**Fix:**  
Emit a Tauri event to the frontend on ICP connection failure so a non-blocking banner can be
shown. Retry on a backoff schedule rather than abandoning permanently for the session.

```rust
// src-tauri/src/icp/sync.rs
Err(e) => {
    eprintln!("[icp] could not build agent: {e}");
    app_handle.emit_all("icp://status", json!({
        "connected": false,
        "message": "Could not reach ICP network. Licence and pattern updates paused."
    })).ok();
    return;
}
```

Frontend listens for `icp://status` and shows a dismissible banner when `connected` is false.

---

## 9. Mutex Panics — `.unwrap()` on Lock Acquisition

**Severity:** High — app stability  
**Files:** `src-tauri/src/torrent/mod.rs`, `src-tauri/src/icp/sync.rs`, `src-tauri/src/commands/mod.rs`

**Problem:**  
Multiple `.unwrap()` calls on `Mutex::lock()` throughout the codebase. If any thread panics
while holding a mutex, the mutex becomes poisoned. All subsequent `.unwrap()` calls on that
same mutex then also panic, cascading failures across threads.

**Fix:**  
Replace every `.unwrap()` on a lock acquisition with `.unwrap_or_else()` that recovers from
poisoning by clearing the poison flag and continuing.

```rust
// Instead of:
let guard = state.lock().unwrap();

// Use:
let guard = state.lock().unwrap_or_else(|poisoned| {
    eprintln!("[warn] mutex was poisoned — recovering");
    poisoned.into_inner()
});
```

Run `cargo clippy -- -D warnings` after making these changes; clippy will catch any remaining
`.unwrap()` calls in non-test code.

---

## 10. Chunk Resume — Integer Underflow on Corrupted Snapshots

**Severity:** High — data integrity  
**File:** `src-tauri/src/download/chunked.rs`

**Problem:**  
When resuming a download, `chunk0_bytes_to_read` is computed as:
```
chunk0_end - chunk0_start + 1 - chunk0_written
```
This is a `u64` subtraction. If `chunk0_written` is somehow larger than `chunk0_end -
chunk0_start + 1` (e.g. the snapshot file is corrupted or the chunk boundaries changed between
versions), the result wraps around to a very large u64. The chunk is then re-requested for
billions of bytes, which will either hang or corrupt the output file.

**Fix:**  
Validate the snapshot before using it. If it fails validation, discard it and restart the
download from scratch rather than resuming.

```rust
// src-tauri/src/download/chunked.rs
let chunk_size = chunk0_end - chunk0_start + 1;
if chunk0_written > chunk_size {
    return Err(format!(
        "corrupted resume snapshot: chunk0_written ({}) > chunk size ({})",
        chunk0_written, chunk_size
    ));
}
let chunk0_bytes_to_read = chunk_size - chunk0_written;
```

---

## 11. VirusTotal Integration — Silent API Failures

**Severity:** Medium — Shield reliability  
**File:** `src-tauri/src/shield/layer1_hash.rs`

**Problem:**  
Any non-404 non-200 response from the VT API (rate limit, invalid key, server error) returns
`Ok(None)`, which the pipeline treats as "not found in VT" — a clean result. A rate-limited
or invalid API key silently disables Layer 1 without any indication.

Additionally, a new `reqwest::Client` is created per hash lookup, meaning each scan pays the
cost of a full TLS handshake.

**Fix:**  
1. Return a distinct error for non-404 failure codes so the pipeline can distinguish "not
   found" from "lookup failed"
2. Use the shared `reqwest::Client` already managed in Tauri state

```rust
// src-tauri/src/shield/layer1_hash.rs
if resp.status() == StatusCode::NOT_FOUND {
    return Ok(None);
}
if !resp.status().is_success() {
    return Err(format!("VirusTotal API error: HTTP {}", resp.status()));
}
```

---

## 12. Sandbox Layer 7 — Silent No-Op on Linux and Windows

**Severity:** Medium — Shield correctness  
**File:** `src-tauri/src/shield/layer7_sandbox.rs`

**Problem:**  
Layer 7 uses `sandbox-exec`, which only exists on macOS. On Linux and Windows the sandbox
command is not found, the error is silently ignored, and the layer returns `Clean` — giving
users a false sense of security. The scan log shows Layer 7 passed when it never ran.

Additionally, if `sandbox-exec` times out the child process is not killed, leaking a running
process.

**Fix:**  
1. Gate Layer 7 behind a compile-time or runtime platform check. On non-macOS, skip the layer
   and mark it as `Unsupported` rather than `Clean`
2. Kill the child process explicitly on timeout

```rust
// src-tauri/src/shield/layer7_sandbox.rs
#[cfg(not(target_os = "macos"))]
pub async fn run(_path: &str) -> LayerResult {
    LayerResult { status: "unsupported".into(), detail: "sandbox-exec is macOS only".into() }
}

#[cfg(target_os = "macos")]
pub async fn run(path: &str) -> LayerResult {
    // existing implementation, but with child.kill() on timeout
}
```

---

## 13. Quarantine — No Transaction Boundary on File Move + DB Write

**Severity:** Medium — data integrity  
**File:** `src-tauri/src/download/mod.rs`

**Problem:**  
When quarantining a file, the file is moved on disk first and then the database is updated. If
the DB write fails, the file is orphaned in the quarantine folder with no record of it. There
is no way to track or recover these orphaned files, and they silently consume disk space.

**Fix:**  
Write the DB record first inside a transaction, then move the file. If the file move fails,
roll back the transaction. This way the DB is always the source of truth.

```rust
// src-tauri/src/download/mod.rs
let tx = conn.transaction()?;
// INSERT quarantine record inside tx
tx.execute("INSERT INTO quarantine (...) VALUES (?1, ?2, ?3)", params![...])?;
// Only move the file after the DB write succeeds
std::fs::rename(&source_path, &quarantine_path)
    .map_err(|e| { tx.rollback().ok(); e.to_string() })?;
tx.commit()?;
```

---

## 14. Device ID — Not Persisted Before Use

**Severity:** Medium — identity stability  
**File:** `src-tauri/src/icp/sync.rs`

**Problem:**  
A new device ID is generated with `Uuid::new_v4()` and immediately used, with the DB write
treated as fire-and-forget via `.ok()`. If the write fails, the app proceeds with an unsaved
ID. On the next startup a new ID is generated, and the device is registered on ICP a second
time under a different ID. The first registration is orphaned.

**Fix:**  
Return an error if the device ID cannot be persisted. Do not proceed with ICP registration
until the ID is confirmed saved.

```rust
// src-tauri/src/icp/sync.rs
let id = uuid::Uuid::new_v4().to_string();
db::set_setting(&conn, "device_id", &id)
    .map_err(|e| format!("could not persist device_id: {e}"))?;
id
```

---

## 15. ~~Cloudflare Worker — Silent Payment Miss on Unknown Customer~~ ✓ RESOLVED

**Resolved by Paystack migration.**  
`invoice.payment` now returns `500` when the subscription code is not found in KV, logging the
event and forcing Paystack to retry. The unknown subscription path is no longer silent.

---

## 16. ~~Cloudflare Worker — No Timeout on ICP Canister Calls~~ ✓ RESOLVED

**Resolved by Paystack migration.**  
`callGrantProWithTimeout()` is implemented using `Promise.race` with a 10-second timeout and is
used for all three event paths (`charge.success`, `invoice.payment`, `subscription.disable`).

---

## 19. Paystack — Geographic Coverage Blocks Most of the World

**Severity:** Critical — business model  
**File:** `worker/index.js`

**Problem:**  
Paystack operates in Nigeria, Ghana, Kenya, South Africa, Egypt, Ivory Coast, and Rwanda.
It does not support payments from the United States, Canada, the United Kingdom, most of
Europe, or most of Asia. A download manager targeting a global audience cannot complete
purchases from the majority of potential users.

Additionally, the currency is hardcoded to ZAR (South African Rand) at R500
(`amount: 50000` in ZAR cents). Users outside South Africa will see an unfamiliar currency,
their banks may apply foreign currency fees, and the real-terms price will fluctuate with the
ZAR exchange rate.

**Fix:**  
Two options — pick one:
1. Add a second payment processor (Stripe, Paddle, or LemonSqueezy) for non-Paystack regions
   and route users to the appropriate checkout based on their country, detected at
   `/init-checkout` via the `CF-IPCountry` header Cloudflare provides for free
2. Switch to Paddle or LemonSqueezy as the primary processor — both handle global payments,
   tax compliance, and currency localisation out of the box, with similar webhook patterns to
   what is already built

Keeping Paystack as-is limits the addressable market to ~6 African countries.

---

## 20. Paystack — Fake Email in Checkout Causes Bad UX and Potential Failures

**Severity:** High — payments  
**File:** `worker/index.js`

**Problem:**  
`handleInitCheckout` constructs a fake email: `` `${device_id}@relay.app` ``. This email is:
- Shown on the Paystack payment page as the customer's email address — users see a UUID where
  they expect their own email, which looks like a broken form
- Used by Paystack to send payment receipts — the receipt goes to a non-existent address
- Potentially rejected by Paystack if they add email domain validation in future

There is no way to associate the payment with the real user without a real email.

**Fix:**  
Ask the user for their email address in the upgrade flow before calling `/init-checkout`, and
pass it through. Store the email alongside the device ID in KV so receipts reach the user and
customer lookup is possible if they need support.

```javascript
// worker/index.js — handleInitCheckout
const { device_id, email } = body
if (!device_id || !email) return new Response('missing device_id or email', { status: 400 })
// use real email in the Paystack initialise call
```

---

## 21. Paystack — `charge.success` Fires for Both New and Renewal Payments

**Severity:** Medium — payments  
**File:** `worker/index.js`

**Problem:**  
Paystack fires `charge.success` for every successful charge — including the first payment and
every subscription renewal. The handler grants Pro for 31 days on every `charge.success`. But
subscription renewals also fire `invoice.payment`, which grants another 31 days. A renewal
therefore triggers two `grant_pro` calls with different event IDs, both of which pass the
idempotency check and both get processed.

The second call overwrites the first with the same or slightly different expiry — not harmful
today — but it creates unnecessary ICP canister calls and could cause subtle timing issues if
both events arrive within seconds of each other with slightly different `Date.now()` values.

**Fix:**  
In the `charge.success` handler, only process events where no `subscription_code` is present
(i.e. one-time purchases). Let `invoice.payment` handle all subscription charges exclusively.

```javascript
// worker/index.js — charge.success handler
if (event.event === 'charge.success') {
  const subCode = event.data.subscription?.subscription_code
  if (subCode) {
    // Subscription charge — let invoice.payment handle this, skip here
    return new Response('ok', { status: 200 })
  }
  // One-time purchase — grant Pro
  ...
}
```

---

## 22. Paystack — No In-App Subscription Cancellation

**Severity:** Medium — user experience  
**File:** `worker/index.js`, settings UI

**Problem:**  
There is no way for a user to cancel their subscription from within ReLay. They would need to
log into Paystack's customer portal directly — an interface most users will not know exists.
When a subscription is cancelled via Paystack's dashboard, `subscription.disable` fires and
the Worker correctly revokes the licence. But if a user can't find how to cancel, they will
dispute the charge with their bank instead, which triggers a chargeback. Chargebacks are
costly and too many will cause Paystack to suspend the merchant account.

**Fix:**  
Add a "Manage subscription" button in the settings Pro panel that calls a new Worker endpoint
(`/manage-subscription`) which uses the Paystack API to return the customer's portal URL or
subscription management link. Open it in the system browser.

---

## 17. Vote Buttons — Stuck Disabled on Failure

**Severity:** Low — community feature  
**File:** `src/scripts/community.js`

**Problem:**  
When a governance vote fails, the approve and reject buttons remain disabled. The user cannot
retry and has no indication the vote didn't go through. The error is only visible in DevTools.

**Fix:**  
Re-enable both buttons on catch and show a toast.

```javascript
// src/scripts/community.js
async function handleVote(proposalId, approve, approveBtn, rejectBtn) {
  try {
    await invoke('vote_proposal', { proposalId, approve })
  } catch (err) {
    showErrorToast(`Vote failed: ${err}`)
    approveBtn.disabled = false
    rejectBtn.disabled = false
  }
}
```

---

## 18. CSS — Undefined Variable on Seeding Status Colour

**Severity:** Low — visual  
**File:** `src/styles/components.css`

**Problem:**  
`.status-seeding` uses `var(--status-complete)` which is never defined. The fallback colour
(likely transparent or inherited) will be used silently, making seeding cards render with the
wrong colour.

**Fix:**  
Replace with the correct token. Based on the rest of the status colours in the file this is
likely `var(--accent-green)` or `var(--color-success)` — verify against `styles/base.css`.

```css
.status-seeding {
  color: var(--accent-green); /* was var(--status-complete) — undefined variable */
}
```

---

## Summary Table

| # | Item | Severity | Area | Status |
|---|------|----------|------|--------|
| 1 | HMAC signature bypass in Worker | Critical | Payments | ✓ Resolved |
| 2 | No idempotency on webhook events | Critical | Payments | ✓ Resolved |
| 3 | Pro licence is a SQLite flag | Critical | Business model | Open |
| 4 | Config panic on startup | Critical | Reliability | Open |
| 5 | Torrent control buttons non-functional | Critical | Core feature | Open |
| 6 | Event subscription cleanup crash | Critical | Download reliability | Open |
| 7 | No user feedback on action failures | High | UX | Open |
| 8 | ICP network failure silent degradation | High | User trust | Open |
| 9 | Mutex `.unwrap()` panics | High | Stability | Open |
| 10 | Chunk resume integer underflow | High | Data integrity | Open |
| 11 | VirusTotal silent API failures | Medium | Shield | Open |
| 12 | Sandbox Layer 7 no-op on Linux/Windows | Medium | Shield | Open |
| 13 | Quarantine no transaction boundary | Medium | Data integrity | Open |
| 14 | Device ID not persisted before use | Medium | Identity | Open |
| 15 | Worker silent miss on unknown customer | Medium | Payments | ✓ Resolved |
| 16 | Worker no timeout on ICP calls | Medium | Reliability | ✓ Resolved |
| 17 | Vote buttons stuck disabled on failure | Low | Community | Open |
| 18 | Undefined CSS variable on seeding colour | Low | Visual | Open |
| 19 | Paystack geographic coverage too narrow | Critical | Business model | Open |
| 20 | Fake email in checkout (bad UX + receipts) | High | Payments | Open |
| 21 | `charge.success` double-processes renewals | Medium | Payments | Open |
| 22 | No in-app subscription cancellation | Medium | UX / Payments | Open |
