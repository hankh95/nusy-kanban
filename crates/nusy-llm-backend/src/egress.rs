//! # EX-4985 — LLM-egress PHI gate (VY-PHI-Trust-Boundary E2)
//!
//! The 2026-06-15 adversarial review named LLM egress **the #1 real breach vector**:
//! [`OpenAiBackend::new`](crate::OpenAiBackend) reads `VLLM_BASE_URL` (else `OPENAI_BASE_URL`,
//! else a localhost default) and POSTs the prompt to `{base_url}/chat/completions` — so a single
//! env var can redirect a **PHI-bearing prompt to a cloud endpoint over plaintext http**, with no
//! check that the prompt is de-identified, the endpoint is BAA-covered, or the transport is TLS.
//!
//! This module is the gate every model call MUST pass — **fail-closed** on each axis:
//!
//! 1. **Endpoint allowlist** ([`EgressPolicy`]) — only BAA-covered / local hosts. An off-list host
//!    is a DENY ([`EgressError::OffListEndpoint`]). The BAA host *set* is E5/Captain-ratified data;
//!    this is the mechanism with a conservative localhost-only default.
//! 2. **TLS-required** — a non-`https` scheme to a non-local host is a DENY
//!    ([`EgressError::PlaintextTransport`]); only an explicitly-allowed localhost dev endpoint may
//!    be plaintext.
//! 3. **De-identification as a hard precondition** — a PHI prompt cannot be sent without a
//!    [`DeIdAttestation`] (produced by the VY-Patient-Privacy Safe-Harbor projector). The gate has
//!    no bypass: a PHI [`GatedPrompt`] without an attestation is a DENY
//!    ([`EgressError::PhiWithoutDeId`]).
//! 4. **Audit** — every egress records `(endpoint, de_id_applied, allowed)` for the E4 aggregation.
//!
//! The de-id projector itself is NOT reimplemented here — it is consumed (the caller de-identifies
//! via `nusy_phi_policy`, then attests). nusy-llm-backend stays generic (no phi-policy dependency).

use std::sync::Mutex;

use crate::{LlmClient, LlmParams, Result};

/// Why an egress was denied — every variant is **fail-closed**.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EgressError {
    /// The resolved endpoint host is not on the BAA/local allowlist.
    #[error("egress DENY: endpoint host '{0}' is not on the BAA/local allowlist")]
    OffListEndpoint(String),
    /// A non-TLS (plaintext http) transport to a non-local host — PHI requires https.
    #[error("egress DENY: plaintext (non-TLS) transport to '{0}' — PHI requires https")]
    PlaintextTransport(String),
    /// A PHI prompt was submitted without a de-identification attestation.
    #[error("egress DENY: PHI prompt without a de-identification attestation")]
    PhiWithoutDeId,
    /// The endpoint URL could not be parsed.
    #[error("egress DENY: malformed endpoint URL '{0}'")]
    MalformedUrl(String),
}

/// The endpoint allowlist + TLS policy. The conservative default ([`localhost_only`]) permits only
/// the local vLLM server; the BAA-covered host set is **E5/Captain-ratified data** added via
/// [`with_allowed_hosts`](Self::with_allowed_hosts) — this crate ships the mechanism, not the list.
#[derive(Debug, Clone)]
pub struct EgressPolicy {
    /// Hosts (no scheme/port) permitted as egress targets.
    allowed_hosts: Vec<String>,
    /// Whether a localhost endpoint may use plaintext http (dev/local vLLM). Non-local hosts always
    /// require https regardless.
    allow_localhost_plaintext: bool,
}

impl EgressPolicy {
    /// Localhost-only (the safe default): local vLLM over http is allowed; everything else DENY.
    pub fn localhost_only() -> Self {
        Self {
            allowed_hosts: vec!["localhost".into(), "127.0.0.1".into(), "::1".into()],
            allow_localhost_plaintext: true,
        }
    }

    /// Add BAA-covered hosts to the allowlist (E5/Captain-ratified set).
    pub fn with_allowed_hosts<I, S>(mut self, hosts: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.allowed_hosts.extend(hosts.into_iter().map(Into::into));
        self
    }

    fn is_local(host: &str) -> bool {
        matches!(host, "localhost" | "127.0.0.1" | "::1")
    }

    /// **Fail-closed** endpoint check: parse the scheme + host, require the host on the allowlist
    /// AND the transport to be https (unless an allowed localhost may be plaintext).
    pub fn check_endpoint(&self, base_url: &str) -> std::result::Result<(), EgressError> {
        let (scheme, host) = parse_scheme_host(base_url)
            .ok_or_else(|| EgressError::MalformedUrl(base_url.to_string()))?;

        if !self.allowed_hosts.iter().any(|h| h == &host) {
            return Err(EgressError::OffListEndpoint(host));
        }
        let https = scheme.eq_ignore_ascii_case("https");
        let plaintext_local_ok = self.allow_localhost_plaintext && Self::is_local(&host);
        if !https && !plaintext_local_ok {
            return Err(EgressError::PlaintextTransport(base_url.to_string()));
        }
        Ok(())
    }
}

/// Parse `scheme` + `host` from a base URL. Minimal (no `url` crate dependency): splits on `://`
/// then takes the authority up to the next `/`, `:`, or end. Returns `None` if there is no scheme.
fn parse_scheme_host(base_url: &str) -> Option<(String, String)> {
    let (scheme, rest) = base_url.split_once("://")?;
    if scheme.is_empty() {
        return None;
    }
    let authority = rest.split(['/', '?', '#']).next().unwrap_or(rest);
    // Strip credentials (`user@host`) and a trailing `:port`.
    let host_port = authority.rsplit('@').next().unwrap_or(authority);
    let host = host_port
        .rsplit_once(':')
        .map(|(h, _)| h)
        .unwrap_or(host_port);
    if host.is_empty() {
        return None;
    }
    Some((scheme.to_string(), host.to_string()))
}

/// Resolve the effective base URL the OpenAI/vLLM backend would POST to — the EXACT value the gate
/// must validate (mirrors [`OpenAiBackend::new`](crate::OpenAiBackend)'s env logic, which is the
/// env-redirect breach vector: `VLLM_BASE_URL` → `OPENAI_BASE_URL` → localhost default).
pub fn resolve_openai_base_url_from_env() -> String {
    std::env::var("VLLM_BASE_URL")
        .or_else(|_| std::env::var("OPENAI_BASE_URL"))
        .unwrap_or_else(|_| "http://localhost:8000/v1".to_string())
}

/// A de-identification **attestation** — proof that a prompt's PHI was stripped by the
/// VY-Patient-Privacy Safe-Harbor projector. Construct ONLY after running that projector
/// ([`attest`](Self::attest)); its presence is the egress gate's de-id precondition. (The type is
/// the bypass-prevention: a PHI [`GatedPrompt`] cannot be built without one.)
#[derive(Debug, Clone, Copy)]
pub struct DeIdAttestation {
    _private: (),
}

impl DeIdAttestation {
    /// Mint an attestation. **Contract:** call this ONLY on text that has passed the Safe-Harbor
    /// de-id projector (`nusy_phi_policy::SafeHarborProjector` / `DeIdentifyingFactStore`). The
    /// egress gate trusts the attestation as the de-id proof.
    pub fn attest() -> Self {
        Self { _private: () }
    }
}

/// A prompt presented to the egress gate. **No raw-PHI bypass:** a prompt that carried PHI must be
/// built via [`de_identified`](Self::de_identified) (which requires a [`DeIdAttestation`]); a
/// PHI prompt without one is rejected by the gate.
#[derive(Debug, Clone)]
pub struct GatedPrompt {
    text: String,
    is_phi: bool,
    attestation: Option<DeIdAttestation>,
}

impl GatedPrompt {
    /// A prompt that carries NO PHI (guideline / system text) — sendable as-is.
    pub fn non_phi(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            is_phi: false,
            attestation: None,
        }
    }

    /// A prompt that DID carry PHI and has been **de-identified** — requires the attestation, so
    /// it cannot be constructed for raw PHI.
    pub fn de_identified(text: impl Into<String>, attestation: DeIdAttestation) -> Self {
        Self {
            text: text.into(),
            is_phi: true,
            attestation: Some(attestation),
        }
    }

    /// A PHI prompt WITHOUT de-id — only for exercising the gate's fail-closed path; the gate
    /// rejects it. (Exists so a caller doing runtime classification has a representable value that
    /// the gate then denies, rather than a silent leak.)
    pub fn phi_undeidentified(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            is_phi: true,
            attestation: None,
        }
    }

    fn de_id_applied(&self) -> bool {
        self.attestation.is_some()
    }
}

/// An egress audit record — consumed by the E4 disclosure-path aggregation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EgressAudit {
    pub endpoint: String,
    pub de_id_applied: bool,
    pub allowed: bool,
    pub deny_reason: Option<String>,
}

/// The egress gate: wraps an [`LlmClient`], pinned to an endpoint validated against an
/// [`EgressPolicy`] at construction. Every call re-checks the endpoint and enforces the de-id
/// precondition; all outcomes (allow/deny) are audited. **Fail-closed throughout.**
pub struct EgressGate<C: LlmClient> {
    inner: C,
    endpoint: String,
    policy: EgressPolicy,
    audit: Mutex<Vec<EgressAudit>>,
}

impl<C: LlmClient> EgressGate<C> {
    /// Wrap a client pinned to `endpoint`. **Fails closed** if the endpoint is off-list or
    /// non-TLS — you cannot even build a gate pointing at a forbidden endpoint.
    pub fn wrap(
        inner: C,
        endpoint: impl Into<String>,
        policy: EgressPolicy,
    ) -> std::result::Result<Self, EgressError> {
        let endpoint = endpoint.into();
        policy.check_endpoint(&endpoint)?;
        Ok(Self {
            inner,
            endpoint,
            policy,
            audit: Mutex::new(Vec::new()),
        })
    }

    /// The accumulated egress audit trail.
    pub fn audit_log(&self) -> Vec<EgressAudit> {
        self.audit.lock().expect("audit lock").clone()
    }

    fn record(&self, de_id_applied: bool, allowed: bool, deny_reason: Option<String>) {
        self.audit.lock().expect("audit lock").push(EgressAudit {
            endpoint: self.endpoint.clone(),
            de_id_applied,
            allowed,
            deny_reason,
        });
    }

    /// Check this prompt may egress: endpoint still valid (belt-and-suspenders) AND a PHI prompt
    /// carries its de-id attestation. Records the decision. Returns the sendable text or a DENY.
    fn authorize<'p>(&self, prompt: &'p GatedPrompt) -> Result<&'p str> {
        // Re-validate the endpoint at call time (defense-in-depth; also catches a mutated policy).
        if let Err(e) = self.policy.check_endpoint(&self.endpoint) {
            self.record(prompt.de_id_applied(), false, Some(e.to_string()));
            return Err(e.into());
        }
        // De-id precondition: a PHI prompt MUST carry an attestation.
        if prompt.is_phi && prompt.attestation.is_none() {
            self.record(false, false, Some(EgressError::PhiWithoutDeId.to_string()));
            return Err(EgressError::PhiWithoutDeId.into());
        }
        self.record(prompt.de_id_applied(), true, None);
        Ok(&prompt.text)
    }

    /// Gated [`LlmClient::complete`]: authorize the prompt, then forward to the inner client.
    pub async fn complete(&self, prompt: &GatedPrompt, params: &LlmParams) -> Result<String> {
        let text = self.authorize(prompt)?;
        self.inner.complete(text, params).await
    }

    /// Gated [`LlmClient::stream`].
    pub async fn stream(&self, prompt: &GatedPrompt, params: &LlmParams) -> Result<Vec<String>> {
        let text = self.authorize(prompt)?;
        self.inner.stream(text, params).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{LlmError, MockLlmBackend};

    fn baa_policy() -> EgressPolicy {
        // localhost (plaintext ok) + a BAA-covered TLS host.
        EgressPolicy::localhost_only().with_allowed_hosts(["vllm.baa.example.com"])
    }

    // ---- endpoint allowlist + TLS (the env-redirect breach) ----

    #[test]
    fn off_list_host_fails_closed() {
        let p = baa_policy();
        assert_eq!(
            p.check_endpoint("https://api.openai.com/v1").unwrap_err(),
            EgressError::OffListEndpoint("api.openai.com".into())
        );
        assert_eq!(
            p.check_endpoint("https://api.anthropic.com").unwrap_err(),
            EgressError::OffListEndpoint("api.anthropic.com".into())
        );
    }

    #[test]
    fn plaintext_http_to_a_non_local_host_fails_closed() {
        let p = baa_policy();
        // The BAA host is allow-listed, but plaintext http to it is denied (PHI requires TLS).
        assert_eq!(
            p.check_endpoint("http://vllm.baa.example.com/v1")
                .unwrap_err(),
            EgressError::PlaintextTransport("http://vllm.baa.example.com/v1".into())
        );
        // https to the same host is fine.
        assert!(p.check_endpoint("https://vllm.baa.example.com/v1").is_ok());
    }

    #[test]
    fn localhost_plaintext_is_allowed_for_local_vllm() {
        let p = baa_policy();
        assert!(p.check_endpoint("http://localhost:8000/v1").is_ok());
        assert!(p.check_endpoint("http://127.0.0.1:8000/v1").is_ok());
    }

    #[test]
    fn gate_cannot_be_built_pointing_at_a_forbidden_endpoint() {
        // The classic breach: VLLM_BASE_URL redirected to a cloud endpoint → gate construction
        // fails closed, so no client can be wired to it. (match, not unwrap_err — EgressGate is
        // intentionally not Debug.)
        match EgressGate::wrap(
            MockLlmBackend::new(),
            "https://api.openai.com/v1",
            baa_policy(),
        ) {
            Err(e) => assert_eq!(e, EgressError::OffListEndpoint("api.openai.com".into())),
            Ok(_) => panic!("gate must fail closed on an off-list endpoint"),
        }
    }

    // ---- de-id as a hard precondition ----

    #[tokio::test]
    async fn a_phi_prompt_without_de_id_attestation_fails_closed() {
        let gate = EgressGate::wrap(
            MockLlmBackend::new(),
            "http://localhost:8000/v1",
            baa_policy(),
        )
        .unwrap();
        let phi = GatedPrompt::phi_undeidentified("Patient John Doe, SSN 123-45-6789");
        let res = gate.complete(&phi, &LlmParams::default()).await;
        assert!(
            matches!(res, Err(LlmError::Egress(EgressError::PhiWithoutDeId))),
            "PHI without de-id must fail closed, got {res:?}"
        );
        // The denial was audited.
        let log = gate.audit_log();
        assert_eq!(log.len(), 1);
        assert!(!log[0].allowed);
        assert_eq!(
            log[0].deny_reason.as_deref(),
            Some("egress DENY: PHI prompt without a de-identification attestation")
        );
    }

    #[tokio::test]
    async fn an_allowlisted_tls_de_identified_call_succeeds_and_audits() {
        let gate = EgressGate::wrap(
            MockLlmBackend::with_responses(vec!["ok".into()]),
            "https://vllm.baa.example.com/v1",
            baa_policy(),
        )
        .unwrap();
        // A de-identified PHI prompt (carries the attestation) → permitted.
        let prompt =
            GatedPrompt::de_identified("Patient [REDACTED-NAME]", DeIdAttestation::attest());
        let out = gate.complete(&prompt, &LlmParams::default()).await.unwrap();
        assert_eq!(out, "ok");
        // A non-PHI guideline prompt → permitted (no attestation needed).
        let g = GatedPrompt::non_phi("What does JNC8 recommend for stage-1 HTN?");
        assert!(gate.complete(&g, &LlmParams::default()).await.is_ok());

        let log = gate.audit_log();
        assert_eq!(log.len(), 2);
        assert!(log.iter().all(|a| a.allowed));
        assert!(
            log[0].de_id_applied,
            "the de-identified PHI call records de_id_applied"
        );
        assert!(!log[1].de_id_applied, "the non-PHI call needed no de-id");
    }

    // NOTE: `resolve_openai_base_url_from_env` is a thin mirror of `OpenAiBackend::new`'s env logic
    // and is deliberately NOT unit-tested here — reading process env in a parallel test would join
    // this crate's existing (racy) `backend_from_env_*` env-var tests. The gate's endpoint checks
    // are exercised above with explicit URLs; the function exists so callers resolve the SAME
    // base_url the backend would, then `EgressPolicy::check_endpoint` it.

    #[test]
    fn parse_scheme_host_handles_ports_paths_and_creds() {
        assert_eq!(
            parse_scheme_host("https://vllm.baa.example.com:8443/v1/chat"),
            Some(("https".into(), "vllm.baa.example.com".into()))
        );
        assert_eq!(
            parse_scheme_host("http://localhost:8000/v1"),
            Some(("http".into(), "localhost".into()))
        );
        assert_eq!(parse_scheme_host("not-a-url"), None);
    }
}
