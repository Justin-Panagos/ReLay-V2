import { HttpAgent, Actor } from '@dfinity/agent'
import { Ed25519KeyIdentity } from '@dfinity/identity'
import { Principal } from '@dfinity/principal'

// Minimal Candid IDL for the identity canister — grant_pro only.
const idlFactory = ({ IDL }) =>
  IDL.Service({ grant_pro: IDL.Func([IDL.Text, IDL.Nat64], [], []) })

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
  const actor = Actor.createActor(idlFactory, {
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
 * Handles POST /init-checkout — initialises a Paystack transaction and returns
 * the authorization_url for the app to open in the system browser.
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
  const { device_id } = body
  if (!device_id) return new Response('missing device_id', { status: 400 })

  const resp = await fetch('https://api.paystack.co/transaction/initialize', {
    method: 'POST',
    headers: {
      Authorization: `Bearer ${env.PAYSTACK_SECRET_KEY}`,
      'Content-Type': 'application/json',
    },
    body: JSON.stringify({
      email: `${device_id}@relay.app`,
      amount: 50000,
      currency: 'ZAR',
      metadata: { device_id },
    }),
  })
  const json = await resp.json()
  if (!json.status) return new Response('Paystack init failed', { status: 502 })
  return new Response(JSON.stringify({ authorization_url: json.data.authorization_url }), {
    headers: { 'Content-Type': 'application/json' },
  })
}

export default {
  /**
   * Main Worker fetch handler.
   * Routes POST /init-checkout and POST /webhook; all other paths return 404.
   *
   * Args:
   *   request: Incoming Request from Cloudflare.
   *   env:     Worker environment bindings (RELAY_LICENCES KV, secrets).
   *
   * Returns:
   *   Response — 200 on success, 400 on bad input, 404 on wrong path, 500 on upstream error.
   */
  async fetch(request, env) {
    const url = new URL(request.url)

    if (request.method === 'POST' && url.pathname === '/init-checkout') {
      return handleInitCheckout(request, env)
    }

    if (request.method !== 'POST' || url.pathname !== '/webhook') {
      return new Response('not found', { status: 404 })
    }

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

    // Idempotency check — uses Paystack event ID, TTL 7 days (Paystack retries for ~3 days).
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
    const canisterId = env.IDENTITY_CANISTER_ID

    try {
      if (event.event === 'charge.success') {
        const deviceId = event.data.metadata?.device_id
        if (!deviceId) return new Response('missing device_id in metadata', { status: 400 })
        const subCode = event.data.subscription?.subscription_code
        if (subCode) {
          await env.RELAY_LICENCES.put(`sub:${subCode}`, deviceId)
        }
        const expiry = Math.floor(Date.now() / 1000) + 31 * 24 * 60 * 60
        await callGrantProWithTimeout(agent, canisterId, deviceId, expiry)

      } else if (event.event === 'invoice.payment') {
        const subCode = event.data.subscription?.subscription_code
        const deviceId = await env.RELAY_LICENCES.get(`sub:${subCode}`)
        if (!deviceId) {
          console.error(JSON.stringify({ event: 'unknown_subscription', sub_code: subCode, event_id: event.id }))
          return new Response('subscription not found', { status: 500 })
        }
        const periodEnd = event.data.paid_at
          ? Math.floor(new Date(event.data.paid_at).getTime() / 1000) + 31 * 24 * 60 * 60
          : Math.floor(Date.now() / 1000) + 31 * 24 * 60 * 60
        await callGrantProWithTimeout(agent, canisterId, deviceId, periodEnd)

      } else if (event.event === 'subscription.disable') {
        const subCode = event.data.subscription_code
        const deviceId = await env.RELAY_LICENCES.get(`sub:${subCode}`)
        if (!deviceId) return new Response('ok', { status: 200 })
        await callGrantProWithTimeout(agent, canisterId, deviceId, 0)
      }
      // All other event types are acknowledged but not acted on.
    } catch (err) {
      console.error('ICP canister call failed:', err)
      return new Response('upstream error', { status: 500 })
    }

    return new Response('ok', { status: 200 })
  },
}
