/**
 * Community tab — displays ICP governance proposals and enables voting.
 * Zero-day submission is handled in quarantine.js via the submit_zero_day command.
 */

import { invoke } from '@tauri-apps/api/tauri'
import { escHtml } from './utils.js'
import { showErrorToast } from './toast.js'

/**
 * Wires the refresh button and the tab-switch listener so proposals reload
 * whenever the Community tab is activated. Called once during app init.
 * Does not perform any network calls itself.
 */
export function initCommunity() {
  document.getElementById('community-refresh-btn')
    ?.addEventListener('click', () => loadCommunity())

  document.querySelector('.panel-tab[data-panel-tab="community"]')
    ?.addEventListener('click', () => loadCommunity())
}

/**
 * Fetches proposals and reputation from the ICP canisters (via Tauri commands)
 * and renders both. Safe to call multiple times — clears the list each time.
 */
export async function loadCommunity() {
  await Promise.all([renderProposals(), loadReputation()])
}

/**
 * Fetches all governance proposals and renders them as cards.
 * Shows the empty state element if no proposals exist.
 */
async function renderProposals() {
  const list = document.getElementById('community-proposals-list')
  const empty = document.getElementById('community-empty')
  if (!list || !empty) return

  list.innerHTML = '<div class="empty-state">Loading\u2026</div>'
  empty.classList.add('hidden')

  let proposals
  try {
    proposals = await invoke('get_proposals')
  } catch (err) {
    console.error('get_proposals failed:', err)
    list.innerHTML = '<div class="empty-state">Failed to load proposals \u2014 try again.</div>'
    return
  }

  list.innerHTML = ''

  if (proposals.length === 0) {
    empty.classList.remove('hidden')
    return
  }

  empty.classList.add('hidden')
  // Show newest first.
  for (const proposal of [...proposals].reverse()) {
    list.appendChild(buildProposalCard(proposal))
  }
}

/**
 * Fetches the current device's reputation score and updates the header display.
 */
async function loadReputation() {
  const scoreEl = document.getElementById('community-rep-score')
  if (!scoreEl) return
  try {
    const score = await invoke('get_reputation')
    scoreEl.textContent = String(score)
  } catch {
    scoreEl.textContent = '\u2014'
  }
}

/**
 * Builds and returns a DOM element representing a single governance proposal.
 * Approve and Reject buttons are disabled for resolved proposals.
 *
 * Args:
 *   proposal: Proposal object from the backend (id, sha256, submitter, approve_votes,
 *             reject_votes, status, created_at).
 *
 * Returns:
 *   The constructed proposal card HTMLElement.
 */
function buildProposalCard(proposal) {
  const card = document.createElement('div')
  card.className = 'proposal-card'
  card.dataset.proposalId = proposal.id

  const isPending = proposal.status === 'pending'
  const statusClass = `proposal-status-${escHtml(proposal.status)}`

  // created_at is Unix nanoseconds from the canister.
  const date = proposal.created_at
    ? new Date(Number(proposal.created_at) / 1_000_000).toLocaleDateString()
    : ''

  card.innerHTML = `
    <div class="proposal-hash" title="${escHtml(proposal.sha256)}">${escHtml(proposal.sha256)}</div>
    <div class="proposal-meta">
      <span class="${statusClass}">${escHtml(proposal.status)}</span>
      <span>${escHtml(date)}</span>
      <span>${escHtml(String(proposal.approve_votes))} approve / ${escHtml(String(proposal.reject_votes))} reject</span>
    </div>
    <div class="proposal-votes">
      <button class="vote-approve-btn" ${isPending ? '' : 'disabled'}>Approve</button>
      <button class="vote-reject-btn"  ${isPending ? '' : 'disabled'}>Reject</button>
    </div>
  `

  const approveBtn = card.querySelector('.vote-approve-btn')
  const rejectBtn  = card.querySelector('.vote-reject-btn')

  approveBtn.addEventListener('click', async () => {
    approveBtn.disabled = true
    rejectBtn.disabled = true
    approveBtn.textContent = 'Submitting\u2026'
    await handleVote(proposal.id, true, approveBtn, rejectBtn)
    await loadCommunity()
  })

  rejectBtn.addEventListener('click', async () => {
    rejectBtn.disabled = true
    approveBtn.disabled = true
    rejectBtn.textContent = 'Submitting\u2026'
    await handleVote(proposal.id, false, approveBtn, rejectBtn)
    await loadCommunity()
  })

  return card
}

/**
 * Invokes the vote_proposal command. On failure, re-enables both buttons and shows a toast.
 *
 * Args:
 *   proposalId: The numeric proposal id.
 *   approve:    true to approve, false to reject.
 *   approveBtn: The approve button element (re-enabled on failure).
 *   rejectBtn:  The reject button element (re-enabled on failure).
 */
async function handleVote(proposalId, approve, approveBtn, rejectBtn) {
  try {
    await invoke('vote_proposal', { proposalId, approve })
  } catch (err) {
    console.error('vote_proposal failed:', err)
    showErrorToast(`Vote failed: ${err}`)
    approveBtn.disabled = false
    rejectBtn.disabled = false
  }
}
