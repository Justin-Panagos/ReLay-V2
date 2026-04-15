import { HttpAgent, Actor } from '@dfinity/agent'
import { Ed25519KeyIdentity } from '@dfinity/identity'
import { Principal } from '@dfinity/principal'

// IDL for the Identity canister — all methods used by this Worker.
const identityIdlFactory = ({ IDL }) =>
  IDL.Service({
    grant_pro:        IDL.Func([IDL.Text, IDL.Nat64], [],         []),
    grant_api_key:    IDL.Func([IDL.Text, IDL.Nat64], [],         []),
    revoke_api_key:   IDL.Func([IDL.Text],            [],         []),
    validate_api_key: IDL.Func([IDL.Text],            [IDL.Bool], ['query']),
  })

// IDL for the Pattern canister — export and delta endpoints.
const patternIdlFactory = ({ IDL }) => {
  const PatternEntry = IDL.Record({
    id:           IDL.Nat64,
    sha256:       IDL.Text,
    threat_level: IDL.Text,
    source:       IDL.Text,
    timestamp:    IDL.Nat64,
  })
  return IDL.Service({
    get_full_export: IDL.Func([], [IDL.Vec(PatternEntry)], ['query']),
    get_delta_since: IDL.Func([IDL.Nat64], [IDL.Vec(PatternEntry)], ['query']),
  })
}

// ── Signature verification ───────────────────────────────────────────────────

/**
 * Verifies a Paystack webhook HMAC-SHA512 signature.
 * Throws if the signature header is missing or does not match.
 *
 * Args:
 *   body:      Raw request body string (must not be parsed before verification).
 *   sigHeader: Value of the x-paystack-signature HTTP header.
 *   secret:    Paystack secret key from Dashboard → Settings → API Keys.
 *
 * Returns:
 *   Parsed Paystack event object.
 */
async function verifyAndParsePaystackEvent(body, sigHeader, secret) {
  if (!sigHeader) throw new Error('missing x-paystack-signature header')
  const encoder = new TextEncoder()
  const key = await crypto.subtle.importKey(
    'raw',
    encoder.encode(secret),
    { name: 'HMAC', hash: 'SHA-512' },
    false,
    ['sign']
  )
  const sigBytes = await crypto.subtle.sign('HMAC', key, encoder.encode(body))
  const computed = Array.from(new Uint8Array(sigBytes))
    .map(b => b.toString(16).padStart(2, '0'))
    .join('')
  if (computed !== sigHeader) throw new Error('invalid paystack signature')
  return JSON.parse(body)
}

// ── ICP agent helpers ────────────────────────────────────────────────────────

/**
 * Builds an authenticated ICP HttpAgent using the Worker's Ed25519 identity.
 *
 * Args:
 *   privateKeyPem: PKCS#8 PEM string exported from `dfx identity export`.
 *
 * Returns:
 *   Configured HttpAgent ready for authenticated update calls to ic0.app.
 */
async function buildIcpAgent(privateKeyPem) {
  const b64 = privateKeyPem.replace(/-----[^-]+-----/g, '').replace(/\s/g, '')
  const raw = Uint8Array.from(atob(b64), c => c.charCodeAt(0))
  // dfx exports PKCS#8 — the last 32 bytes are the raw Ed25519 seed.
  const seed = raw.slice(raw.length - 32)
  const identity = Ed25519KeyIdentity.generate(seed)
  return new HttpAgent({ identity, host: 'https://ic0.app' })
}

/**
 * Calls grant_pro on the ICP Identity Canister to set or revoke a Pro licence.
 *
 * Args:
 *   agent:           Authenticated HttpAgent from buildIcpAgent.
 *   canisterId:      Text principal of the identity canister.
 *   deviceId:        Device UUID to activate or revoke.
 *   expiryTimestamp: Unix seconds; 0 revokes the licence.
 */
async function callGrantPro(agent, canisterId, deviceId, expiryTimestamp) {
  const actor = Actor.createActor(identityIdlFactory, {
    agent,
    canisterId: Principal.fromText(canisterId),
  })
  await actor.grant_pro(deviceId, BigInt(expiryTimestamp))
}

/**
 * Wraps callGrantPro with a 10-second timeout via Promise.race.
 * Throws if the ICP call does not resolve within the timeout.
 *
 * Args:
 *   agent:           Authenticated HttpAgent.
 *   canisterId:      Text principal of the identity canister.
 *   deviceId:        Device UUID to activate or revoke.
 *   expiry:          Unix seconds; 0 revokes the licence.
 *   ms:              Timeout in milliseconds (default: 10000).
 */
async function callGrantProWithTimeout(agent, canisterId, deviceId, expiry, ms = 10000) {
  const timeout = new Promise((_, reject) =>
    setTimeout(() => reject(new Error('ICP call timed out after 10s')), ms)
  )
  return Promise.race([callGrantPro(agent, canisterId, deviceId, expiry), timeout])
}

/**
 * Calls grant_api_key on the Identity canister to activate or extend a Threat
 * Intelligence API key. Wraps with a 10-second timeout.
 *
 * Args:
 *   agent:     Authenticated HttpAgent from buildIcpAgent.
 *   canisterId: Text principal of the identity canister.
 *   apiKey:    64-char hex string.
 *   expiry:    Unix seconds at which the key expires.
 *   ms:        Timeout in milliseconds (default: 10000).
 */
async function callGrantApiKey(agent, canisterId, apiKey, expiry, ms = 10000) {
  const actor = Actor.createActor(identityIdlFactory, {
    agent,
    canisterId: Principal.fromText(canisterId),
  })
  const timeout = new Promise((_, reject) =>
    setTimeout(() => reject(new Error('ICP call timed out after 10s')), ms)
  )
  return Promise.race([actor.grant_api_key(apiKey, BigInt(expiry)), timeout])
}

/**
 * Calls revoke_api_key on the Identity canister to immediately invalidate a key.
 * Wraps with a 10-second timeout.
 *
 * Args:
 *   agent:     Authenticated HttpAgent from buildIcpAgent.
 *   canisterId: Text principal of the identity canister.
 *   apiKey:    64-char hex string.
 *   ms:        Timeout in milliseconds (default: 10000).
 */
async function callRevokeApiKey(agent, canisterId, apiKey, ms = 10000) {
  const actor = Actor.createActor(identityIdlFactory, {
    agent,
    canisterId: Principal.fromText(canisterId),
  })
  const timeout = new Promise((_, reject) =>
    setTimeout(() => reject(new Error('ICP call timed out after 10s')), ms)
  )
  return Promise.race([actor.revoke_api_key(apiKey), timeout])
}

/**
 * Validates an API key via a query call to the Identity canister.
 * Uses an anonymous (unauthenticated) agent — query calls do not require auth.
 *
 * Args:
 *   canisterId: Text principal of the identity canister.
 *   apiKey:    64-char hex string to validate.
 *   ms:        Timeout in milliseconds (default: 10000).
 *
 * Returns:
 *   boolean — true if the key is active and not expired.
 */
async function callValidateApiKey(canisterId, apiKey, ms = 10000) {
  const agent = new HttpAgent({ host: 'https://ic0.app' })
  const actor = Actor.createActor(identityIdlFactory, {
    agent,
    canisterId: Principal.fromText(canisterId),
  })
  const timeout = new Promise((_, reject) =>
    setTimeout(() => reject(new Error('ICP call timed out after 10s')), ms)
  )
  return Promise.race([actor.validate_api_key(apiKey), timeout])
}

/**
 * Fetches every entry in the Pattern canister ledger for a snapshot export.
 * Uses an anonymous agent — get_full_export is a public query.
 *
 * Args:
 *   canisterId: Text principal of the pattern canister.
 *   ms:        Timeout in milliseconds (default: 30000 — exports can be large).
 *
 * Returns:
 *   Array of PatternEntry objects.
 */
async function fetchFullExport(canisterId, ms = 30000) {
  const agent = new HttpAgent({ host: 'https://ic0.app' })
  const actor = Actor.createActor(patternIdlFactory, {
    agent,
    canisterId: Principal.fromText(canisterId),
  })
  const timeout = new Promise((_, reject) =>
    setTimeout(() => reject(new Error('ICP export timed out after 30s')), ms)
  )
  return Promise.race([actor.get_full_export(), timeout])
}

/**
 * Fetches pattern entries newer than sinceId from the Pattern canister.
 * Uses an anonymous agent — get_delta_since is a public query.
 *
 * Args:
 *   canisterId: Text principal of the pattern canister.
 *   sinceId:   Return entries with id > sinceId. Pass 0 for all entries.
 *   ms:        Timeout in milliseconds (default: 15000).
 *
 * Returns:
 *   Array of PatternEntry objects.
 */
async function fetchDeltaSince(canisterId, sinceId, ms = 15000) {
  const agent = new HttpAgent({ host: 'https://ic0.app' })
  const actor = Actor.createActor(patternIdlFactory, {
    agent,
    canisterId: Principal.fromText(canisterId),
  })
  const timeout = new Promise((_, reject) =>
    setTimeout(() => reject(new Error('ICP delta timed out after 15s')), ms)
  )
  return Promise.race([actor.get_delta_since(BigInt(sinceId)), timeout])
}

// ── Export format helpers ────────────────────────────────────────────────────

/**
 * Converts an array of PatternEntry objects to a JSON string.
 * BigInt fields (id, timestamp) are serialised as numbers.
 *
 * Args:
 *   entries: Array of PatternEntry from the canister.
 *
 * Returns:
 *   JSON string with a top-level { count, entries } envelope.
 */
function formatAsJson(entries) {
  const normalised = entries.map(e => ({
    id:           Number(e.id),
    sha256:       e.sha256,
    threat_level: e.threat_level,
    source:       e.source,
    timestamp:    Number(e.timestamp),
  }))
  return JSON.stringify({ count: normalised.length, entries: normalised }, null, 2)
}

/**
 * Converts an array of PatternEntry objects to YARA rule format.
 * Each entry becomes a rule named relay_<id> with the sha256 in metadata.
 * The condition is always false — rules are used for hash-lookup only.
 *
 * Args:
 *   entries: Array of PatternEntry from the canister.
 *
 * Returns:
 *   Multi-rule YARA file as a string.
 */
function formatAsYara(entries) {
  const header = [
    '/* ReLay Shield Threat Database */',
    `/* Exported: ${new Date().toISOString()} */`,
    `/* Total rules: ${entries.length} */`,
    '',
  ].join('\n')
  const rules = entries.map(e => {
    const id = Number(e.id)
    return [
      `rule relay_${id} {`,
      `  meta:`,
      `    sha256       = "${e.sha256}"`,
      `    threat_level = "${e.threat_level}"`,
      `    source       = "${e.source}"`,
      `    timestamp    = ${Number(e.timestamp)}`,
      `  condition:`,
      `    false`,
      `}`,
    ].join('\n')
  })
  return header + rules.join('\n\n')
}

/**
 * Converts an array of PatternEntry objects to CSV format.
 *
 * Args:
 *   entries: Array of PatternEntry from the canister.
 *
 * Returns:
 *   CSV string with header row: id,sha256,threat_level,source,timestamp
 */
function formatAsCsv(entries) {
  const rows = entries.map(e =>
    [Number(e.id), e.sha256, e.threat_level, e.source, Number(e.timestamp)].join(',')
  )
  return ['id,sha256,threat_level,source,timestamp', ...rows].join('\n')
}

/**
 * Returns a Response with the correct Content-Type for the requested format.
 *
 * Args:
 *   entries: Array of PatternEntry from the canister.
 *   format:  "json" | "yara" | "csv" (defaults to "json").
 *
 * Returns:
 *   Response with formatted body and Content-Type header.
 */
function exportResponse(entries, format) {
  if (format === 'yara') {
    return new Response(formatAsYara(entries), {
      headers: { 'Content-Type': 'text/plain', 'Content-Disposition': 'attachment; filename="relay_threats.yar"' },
    })
  }
  if (format === 'csv') {
    return new Response(formatAsCsv(entries), {
      headers: { 'Content-Type': 'text/csv', 'Content-Disposition': 'attachment; filename="relay_threats.csv"' },
    })
  }
  return new Response(formatAsJson(entries), {
    headers: { 'Content-Type': 'application/json' },
  })
}

// ── Checkout handlers ────────────────────────────────────────────────────────

/**
 * Handles POST /init-checkout — initialises a Paystack Pro subscription checkout
 * and returns the authorization_url for the app to open in the system browser.
 * The device_id is embedded in transaction metadata so the webhook can map
 * the completed payment back to this device.
 *
 * Args:
 *   request: Incoming POST request with JSON body { device_id: string }.
 *   env:     Worker environment bindings (PAYSTACK_SECRET_KEY).
 *
 * Returns:
 *   JSON response { authorization_url: string } on success, error Response otherwise.
 */
async function handleInitCheckout(request, env) {
  let body
  try {
    body = await request.json()
  } catch {
    return new Response('invalid JSON body', { status: 400 })
  }
  const { device_id, email } = body
  if (!device_id) return new Response('missing device_id', { status: 400 })
  if (!email) return new Response('missing email', { status: 400 })

  const resp = await fetch('https://api.paystack.co/transaction/initialize', {
    method: 'POST',
    headers: {
      Authorization: `Bearer ${env.PAYSTACK_SECRET_KEY}`,
      'Content-Type': 'application/json',
    },
    body: JSON.stringify({
      email,
      amount: 50000,
      currency: 'ZAR',
      metadata: { device_id, type: 'pro' },
      callback_url: `${new URL(request.url).origin}/api/callback`,
    }),
  })
  const json = await resp.json()
  if (!json.status) return new Response('Paystack init failed', { status: 502 })
  return new Response(JSON.stringify({ authorization_url: json.data.authorization_url }), {
    headers: { 'Content-Type': 'application/json' },
  })
}

/**
 * Handles POST /snapshot-checkout — initialises a Paystack one-time $10 payment
 * for a full threat database snapshot. No device_id needed — the snapshot token
 * is the identifier. The callback URL directs the developer to /api/callback
 * where their token is displayed after payment.
 *
 * Args:
 *   request: Incoming POST request with JSON body { email: string }.
 *   env:     Worker environment bindings (PAYSTACK_SECRET_KEY).
 *
 * Returns:
 *   JSON response { authorization_url: string } on success, error Response otherwise.
 */
async function handleSnapshotCheckout(request, env) {
  let body
  try {
    body = await request.json()
  } catch {
    return new Response('invalid JSON body', { status: 400 })
  }
  const { email } = body
  if (!email) return new Response('missing email', { status: 400 })

  const workerOrigin = new URL(request.url).origin
  const resp = await fetch('https://api.paystack.co/transaction/initialize', {
    method: 'POST',
    headers: {
      Authorization: `Bearer ${env.PAYSTACK_SECRET_KEY}`,
      'Content-Type': 'application/json',
    },
    body: JSON.stringify({
      email,
      amount: 100000, // ZAR cents — R100 (~$10 USD at time of writing)
      currency: 'ZAR',
      metadata: { type: 'snapshot' },
      callback_url: `${workerOrigin}/api/callback`,
    }),
  })
  const json = await resp.json()
  if (!json.status) return new Response('Paystack init failed', { status: 502 })
  return new Response(JSON.stringify({ authorization_url: json.data.authorization_url }), {
    headers: { 'Content-Type': 'application/json' },
  })
}

/**
 * Handles POST /api-checkout — initialises a Paystack subscription checkout for
 * monthly ($15), annual ($120), or enterprise ($500) API feed access.
 * The plan code is read from Worker secrets (PAYSTACK_MONTHLY_PLAN etc.).
 *
 * Args:
 *   request: Incoming POST request with JSON body { plan: "monthly"|"annual"|"enterprise", email: string }.
 *   env:     Worker environment bindings (PAYSTACK_SECRET_KEY, plan code secrets).
 *
 * Returns:
 *   JSON response { authorization_url: string } on success, error Response otherwise.
 */
async function handleApiCheckout(request, env) {
  let body
  try {
    body = await request.json()
  } catch {
    return new Response('invalid JSON body', { status: 400 })
  }
  const { plan, email } = body
  if (!email) return new Response('missing email', { status: 400 })

  const planAmounts = { monthly: 150000, annual: 1200000, enterprise: 5000000 }
  const planSecrets = {
    monthly:    env.PAYSTACK_MONTHLY_PLAN,
    annual:     env.PAYSTACK_ANNUAL_PLAN,
    enterprise: env.PAYSTACK_ENTERPRISE_PLAN,
  }
  const amount = planAmounts[plan]
  const planCode = planSecrets[plan]
  if (!amount || !planCode) return new Response('invalid plan — use monthly, annual, or enterprise', { status: 400 })

  const workerOrigin = new URL(request.url).origin
  const resp = await fetch('https://api.paystack.co/transaction/initialize', {
    method: 'POST',
    headers: {
      Authorization: `Bearer ${env.PAYSTACK_SECRET_KEY}`,
      'Content-Type': 'application/json',
    },
    body: JSON.stringify({
      email,
      amount,
      currency: 'ZAR',
      plan: planCode,
      metadata: { type: 'api_key', plan },
      callback_url: `${workerOrigin}/api/callback`,
    }),
  })
  const json = await resp.json()
  if (!json.status) return new Response('Paystack init failed', { status: 502 })
  return new Response(JSON.stringify({ authorization_url: json.data.authorization_url }), {
    headers: { 'Content-Type': 'application/json' },
  })
}

// ── Webhook handler ──────────────────────────────────────────────────────────

/**
 * Generates a cryptographically random 64-character hex string.
 *
 * Returns:
 *   64-char hex string suitable for use as an API key or snapshot token.
 */
function generateHexToken() {
  const bytes = new Uint8Array(32)
  crypto.getRandomValues(bytes)
  return Array.from(bytes).map(b => b.toString(16).padStart(2, '0')).join('')
}

/**
 * Handles POST /webhook — verifies the Paystack HMAC-SHA512 signature, enforces
 * idempotency via KV, and dispatches each event to the correct handler.
 *
 * Args:
 *   request: Incoming POST request from Paystack.
 *   env:     Worker environment bindings (RELAY_LICENCES KV, secrets).
 *
 * Returns:
 *   Response — 200 on success, 400 on bad signature, 500 on upstream error.
 */
async function handleWebhook(request, env) {
  const body = await request.text()
  let event
  try {
    event = await verifyAndParsePaystackEvent(
      body,
      request.headers.get('x-paystack-signature'),
      env.PAYSTACK_SECRET_KEY
    )
  } catch (err) {
    return new Response(`invalid signature: ${err.message}`, { status: 400 })
  }

  // Idempotency — Paystack event ID, 7-day TTL (Paystack retries for ~3 days).
  const idempotencyKey = `processed:${event.id}`
  if (await env.RELAY_LICENCES.get(idempotencyKey)) {
    return new Response('ok', { status: 200 })
  }
  await env.RELAY_LICENCES.put(idempotencyKey, '1', { expirationTtl: 604800 })

  let agent
  try {
    agent = await buildIcpAgent(env.WORKER_PRIVATE_KEY_PEM)
  } catch (err) {
    console.error('failed to build ICP agent:', err)
    return new Response('agent init error', { status: 500 })
  }
  const identityId = env.IDENTITY_CANISTER_ID
  const patternId  = env.PATTERN_CANISTER_ID

  try {
    if (event.event === 'charge.success') {
      const type = event.data.metadata?.type
      const ref  = event.data.reference

      if (type === 'snapshot') {
        // One-time snapshot purchase — generate single-use token, valid 25 hours.
        const token = generateHexToken()
        const ttl = 25 * 60 * 60
        await env.RELAY_LICENCES.put(`snap:${token}`, 'unused', { expirationTtl: ttl })
        // Map Paystack reference → token so /api/callback can look it up.
        await env.RELAY_LICENCES.put(`ref:${ref}`, JSON.stringify({ type: 'snapshot', token }), { expirationTtl: ttl })

      } else if (type === 'api_key') {
        // API feed subscription — generate persistent key.
        const plan    = event.data.metadata?.plan ?? 'monthly'
        const apiKey  = generateHexToken()
        const subCode = event.data.subscription?.subscription_code
        const expiry  = expiryForPlan(plan)

        if (subCode) {
          await env.RELAY_LICENCES.put(`api_sub:${subCode}`, apiKey)
          await env.RELAY_LICENCES.put(`api_plan:${subCode}`, plan)
        }
        await env.RELAY_LICENCES.put(`api:${apiKey}`, 'active')
        // Map reference → key so /api/callback can display it.
        await env.RELAY_LICENCES.put(`ref:${ref}`, JSON.stringify({ type: 'api_key', apiKey }), { expirationTtl: 25 * 60 * 60 })
        await callGrantApiKey(agent, identityId, apiKey, expiry)

      } else {
        // Pro subscription (device_id present, no type or type === 'pro').
        const deviceId = event.data.metadata?.device_id
        if (!deviceId) return new Response('missing device_id in metadata', { status: 400 })
        const subCode = event.data.subscription?.subscription_code
        if (subCode) {
          // If the sub code is already in KV this is a renewal — invoice.payment handles it.
          const existing = await env.RELAY_LICENCES.get(`sub:${subCode}`)
          if (existing) return new Response('ok', { status: 200 })
          await env.RELAY_LICENCES.put(`sub:${subCode}`, deviceId)
          // Reverse lookup: device_id → sub_code for in-app cancellation.
          await env.RELAY_LICENCES.put(`sub_lookup:${deviceId}`, subCode)
        }
        const expiry = Math.floor(Date.now() / 1000) + 31 * 24 * 60 * 60
        await callGrantProWithTimeout(agent, identityId, deviceId, expiry)
      }

    } else if (event.event === 'invoice.payment') {
      const subCode = event.data.subscription?.subscription_code

      // Check if this is an API key subscription renewal.
      const apiKey = await env.RELAY_LICENCES.get(`api_sub:${subCode}`)
      if (apiKey) {
        const plan   = await env.RELAY_LICENCES.get(`api_plan:${subCode}`) ?? 'monthly'
        const expiry = expiryForPlan(plan)
        await callGrantApiKey(agent, identityId, apiKey, expiry)
        return new Response('ok', { status: 200 })
      }

      // Otherwise treat as Pro subscription renewal.
      const deviceId = await env.RELAY_LICENCES.get(`sub:${subCode}`)
      if (!deviceId) {
        console.error(JSON.stringify({ event: 'unknown_subscription', sub_code: subCode, event_id: event.id }))
        return new Response('subscription not found', { status: 500 })
      }
      const periodEnd = event.data.paid_at
        ? Math.floor(new Date(event.data.paid_at).getTime() / 1000) + 31 * 24 * 60 * 60
        : Math.floor(Date.now() / 1000) + 31 * 24 * 60 * 60
      await callGrantProWithTimeout(agent, identityId, deviceId, periodEnd)

    } else if (event.event === 'subscription.disable') {
      const subCode = event.data.subscription_code

      // Check if this is an API key cancellation.
      const apiKey = await env.RELAY_LICENCES.get(`api_sub:${subCode}`)
      if (apiKey) {
        await env.RELAY_LICENCES.delete(`api:${apiKey}`)
        await callRevokeApiKey(agent, identityId, apiKey)
        return new Response('ok', { status: 200 })
      }

      // Otherwise treat as Pro cancellation.
      const deviceId = await env.RELAY_LICENCES.get(`sub:${subCode}`)
      if (!deviceId) return new Response('ok', { status: 200 })
      await callGrantProWithTimeout(agent, identityId, deviceId, 0)
    }
  } catch (err) {
    console.error('ICP canister call failed:', err)
    return new Response('upstream error', { status: 500 })
  }

  return new Response('ok', { status: 200 })
}

/**
 * Returns a Unix-seconds expiry timestamp for the given API plan.
 *
 * Args:
 *   plan: "monthly" | "annual" | "enterprise"
 *
 * Returns:
 *   Unix seconds for the end of the billing period.
 */
function expiryForPlan(plan) {
  const now = Math.floor(Date.now() / 1000)
  if (plan === 'annual' || plan === 'enterprise') {
    return now + 365 * 24 * 60 * 60
  }
  return now + 31 * 24 * 60 * 60
}

// ── Public API endpoints ─────────────────────────────────────────────────────

/**
 * Handles GET /api/export?token=TOKEN&format=json|yara|csv
 * Validates the single-use snapshot token, marks it consumed, fetches the full
 * pattern ledger from the Pattern canister, and returns it in the requested format.
 *
 * Args:
 *   request: Incoming GET request with token and format query params.
 *   env:     Worker environment bindings (RELAY_LICENCES KV, PATTERN_CANISTER_ID).
 *
 * Returns:
 *   Formatted export Response or an error Response.
 */
async function handleApiExport(request, env) {
  const url    = new URL(request.url)
  const token  = url.searchParams.get('token')
  const format = url.searchParams.get('format') ?? 'json'

  if (!token) return new Response('missing token', { status: 400 })

  // Check the token has not already been used.
  if (await env.RELAY_LICENCES.get(`snap_used:${token}`)) {
    return new Response('token already used', { status: 403 })
  }
  // Check the token exists and is still valid.
  if (!await env.RELAY_LICENCES.get(`snap:${token}`)) {
    return new Response('invalid or expired token', { status: 403 })
  }

  // Mark as used before fetching — prevents double-use in concurrent requests.
  await env.RELAY_LICENCES.put(`snap_used:${token}`, '1', { expirationTtl: 25 * 60 * 60 })

  let entries
  try {
    entries = await fetchFullExport(env.PATTERN_CANISTER_ID)
  } catch (err) {
    console.error('pattern canister export failed:', err)
    return new Response('export failed — try again later', { status: 502 })
  }

  return exportResponse(entries, format)
}

/**
 * Handles GET /api/delta?since=N&api_key=KEY&format=json|yara|csv
 * Validates the API key against the Identity canister, then returns all Pattern
 * entries with id > since from the Pattern canister.
 *
 * Args:
 *   request: Incoming GET request with since, api_key, and format query params.
 *   env:     Worker environment bindings.
 *
 * Returns:
 *   Formatted delta Response or an error Response.
 */
async function handleApiDelta(request, env) {
  const url    = new URL(request.url)
  const apiKey = url.searchParams.get('api_key')
  const since  = parseInt(url.searchParams.get('since') ?? '0', 10)
  const format = url.searchParams.get('format') ?? 'json'

  if (!apiKey) return new Response('missing api_key', { status: 400 })

  let valid
  try {
    valid = await callValidateApiKey(env.IDENTITY_CANISTER_ID, apiKey)
  } catch (err) {
    console.error('validate_api_key failed:', err)
    return new Response('could not validate key — try again later', { status: 502 })
  }
  if (!valid) return new Response('invalid or expired api_key', { status: 403 })

  let entries
  try {
    entries = await fetchDeltaSince(env.PATTERN_CANISTER_ID, isNaN(since) ? 0 : since)
  } catch (err) {
    console.error('pattern canister delta failed:', err)
    return new Response('delta fetch failed — try again later', { status: 502 })
  }

  return exportResponse(entries, format)
}

/**
 * Handles GET /api/callback?reference=TRX_REF
 * Called by Paystack after a completed payment. Looks up the Paystack transaction
 * reference in KV and returns an HTML page showing the snapshot token or API key.
 *
 * Args:
 *   request: Incoming GET request with reference query param.
 *   env:     Worker environment bindings (RELAY_LICENCES KV).
 *
 * Returns:
 *   HTML page with the credential, or an error Response.
 */
async function handleApiCallback(request, env) {
  const ref = new URL(request.url).searchParams.get('reference')
  if (!ref) return new Response('missing reference', { status: 400 })

  const raw = await env.RELAY_LICENCES.get(`ref:${ref}`)
  if (!raw) return new Response('payment not found or already retrieved', { status: 404 })

  const data = JSON.parse(raw)

  let heading, label, value, note
  if (data.type === 'snapshot') {
    heading = 'Your Snapshot Download Token'
    label   = 'Token'
    value   = data.token
    note    = 'This token is valid for 24 hours and can only be used once.<br>Use it to download the full ReLay threat database.'
  } else {
    heading = 'Your Threat Intelligence API Key'
    label   = 'API Key'
    value   = data.apiKey
    note    = 'Keep this key secret. Use it with the <code>/api/delta</code> endpoint to receive pattern updates.'
  }

  const html = `<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="UTF-8">
  <title>ReLay Threat Intelligence</title>
  <style>
    body { font-family: system-ui, sans-serif; max-width: 600px; margin: 60px auto; padding: 0 20px; }
    h1 { font-size: 1.4rem; margin-bottom: 8px; }
    .credential { background: #f4f4f4; border-radius: 6px; padding: 16px; font-family: monospace; font-size: 0.95rem; word-break: break-all; margin: 16px 0; }
    .note { color: #555; font-size: 0.9rem; }
  </style>
</head>
<body>
  <h1>ReLay Threat Intelligence</h1>
  <h2>${heading}</h2>
  <p>${label}:</p>
  <div class="credential">${value}</div>
  <p class="note">${note}</p>
</body>
</html>`

  return new Response(html, { headers: { 'Content-Type': 'text/html' } })
}

// ── Main router ──────────────────────────────────────────────────────────────

/**
 * Cancels a Pro subscription on behalf of a device.
 * Looks up the subscription code by device_id, calls the Paystack Subscription disable
 * endpoint to cancel it, and lets the subsequent subscription.disable webhook revoke
 * the licence on ICP.
 *
 * Args:
 *   request: Incoming POST request with JSON body { device_id }.
 *   env:     Worker environment bindings (PAYSTACK_SECRET_KEY, RELAY_LICENCES KV).
 *
 * Returns:
 *   JSON { ok: true } on success, error Response otherwise.
 */
async function handleCancelSubscription(request, env) {
  let body
  try {
    body = await request.json()
  } catch {
    return new Response('invalid JSON body', { status: 400 })
  }
  const { device_id } = body
  if (!device_id) return new Response('missing device_id', { status: 400 })

  const subCode = await env.RELAY_LICENCES.get(`sub_lookup:${device_id}`)
  if (!subCode) return new Response('no active subscription found', { status: 404 })

  // Retrieve the subscription to get the email_token required by Paystack's disable endpoint.
  const subResp = await fetch(`https://api.paystack.co/subscription/${subCode}`, {
    headers: { Authorization: `Bearer ${env.PAYSTACK_SECRET_KEY}` },
  })
  const subJson = await subResp.json()
  if (!subJson.status) return new Response('could not retrieve subscription details', { status: 502 })

  const emailToken = subJson.data?.email_token
  if (!emailToken) return new Response('subscription has no email_token', { status: 502 })

  const disableResp = await fetch('https://api.paystack.co/subscription/disable', {
    method: 'POST',
    headers: {
      Authorization: `Bearer ${env.PAYSTACK_SECRET_KEY}`,
      'Content-Type': 'application/json',
    },
    body: JSON.stringify({ code: subCode, token: emailToken }),
  })
  const disableJson = await disableResp.json()
  if (!disableJson.status) return new Response('Paystack disable failed', { status: 502 })

  return new Response(JSON.stringify({ ok: true }), {
    headers: { 'Content-Type': 'application/json' },
  })
}

export default {
  /**
   * Main Worker fetch handler.
   * Routes all incoming requests to the appropriate handler by method and path.
   *
   * Args:
   *   request: Incoming Request from Cloudflare.
   *   env:     Worker environment bindings (RELAY_LICENCES KV, secrets).
   *
   * Returns:
   *   Response — delegated to the matched handler, or 404.
   */
  async fetch(request, env) {
    const url    = new URL(request.url)
    const method = request.method
    const path   = url.pathname

    if (method === 'POST' && path === '/init-checkout')          return handleInitCheckout(request, env)
    if (method === 'POST' && path === '/snapshot-checkout')      return handleSnapshotCheckout(request, env)
    if (method === 'POST' && path === '/api-checkout')           return handleApiCheckout(request, env)
    if (method === 'POST' && path === '/cancel-subscription')    return handleCancelSubscription(request, env)
    if (method === 'POST' && path === '/webhook')                return handleWebhook(request, env)
    if (method === 'GET'  && path === '/api/export')        return handleApiExport(request, env)
    if (method === 'GET'  && path === '/api/delta')         return handleApiDelta(request, env)
    if (method === 'GET'  && path === '/api/callback')      return handleApiCallback(request, env)
    return new Response('not found', { status: 404 })
  },
}
