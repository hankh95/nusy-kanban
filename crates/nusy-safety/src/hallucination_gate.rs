//! Hallucination gate — pre-query validation with KBDD integration.
//!
//! EX-3154 Phase 4: Ports V12 `hallucination_gate.py` to Rust. The gate
//! checks whether a query is covered by KBDD scenarios before allowing
//! a response. Uncovered queries in safety-critical domains are refused.

use crate::battery::RiskTier;

/// Coverage checker function type — returns (covered, cq_score) for a query.
type CoverageFn = Box<dyn Fn(&str) -> (bool, f64) + Send + Sync>;

/// Result of a hallucination gate check.
#[derive(Debug, Clone)]
pub struct GateResult {
    /// Whether the response is allowed.
    pub allowed: bool,
    /// Reason for the decision.
    pub reason: String,
    /// Whether the query was covered by KBDD scenarios.
    pub kbdd_covered: Option<bool>,
    /// CQ score at time of check (if KBDD available).
    pub cq_score: Option<f64>,
}

/// Hallucination gate — validates queries before allowing responses.
///
/// For safety-critical domains (T0), queries must be covered by KBDD
/// scenarios. For lower-risk domains, the gate is more permissive.
pub struct HallucinationGate {
    risk_tier: RiskTier,
    /// KBDD coverage checker — returns (covered, cq_score) for a query.
    coverage_fn: Option<CoverageFn>,
}

impl HallucinationGate {
    /// Create a gate for the given domain.
    pub fn for_domain(domain: &str) -> Self {
        Self {
            risk_tier: RiskTier::for_domain(domain),
            coverage_fn: None,
        }
    }

    /// Attach a KBDD coverage checker.
    ///
    /// The function receives a query string and returns `(covered, cq_score)`
    /// where `covered` is true if the query is within KBDD scenario coverage,
    /// and `cq_score` is the current Comprehension Quotient.
    pub fn with_kbdd_coverage(mut self, coverage_fn: CoverageFn) -> Self {
        self.coverage_fn = Some(coverage_fn);
        self
    }

    /// Check whether a query should be allowed.
    pub fn check(&self, query: &str) -> GateResult {
        // T3 creative: always allow
        if self.risk_tier == RiskTier::Creative {
            return GateResult {
                allowed: true,
                reason: "T3 creative domain — hallucination acceptable".into(),
                kbdd_covered: None,
                cq_score: None,
            };
        }

        // If KBDD coverage checker is available, use it
        if let Some(ref check_fn) = self.coverage_fn {
            let (covered, cq) = check_fn(query);

            if covered {
                return GateResult {
                    allowed: true,
                    reason: format!("Query covered by KBDD scenarios (CQ={cq:.2})"),
                    kbdd_covered: Some(true),
                    cq_score: Some(cq),
                };
            }

            // Not covered — decision depends on risk tier
            match self.risk_tier {
                RiskTier::SafetyCritical => GateResult {
                    allowed: false,
                    reason: format!(
                        "T0 safety-critical: query not covered by KBDD (CQ={cq:.2}). \
                         Refusing to prevent potential hallucination."
                    ),
                    kbdd_covered: Some(false),
                    cq_score: Some(cq),
                },
                RiskTier::Professional => {
                    // T1: allow if CQ is high enough (>0.8)
                    let allowed = cq >= 0.8;
                    GateResult {
                        allowed,
                        reason: if allowed {
                            format!("T1 professional: not covered but CQ={cq:.2} >= 0.8")
                        } else {
                            format!(
                                "T1 professional: not covered and CQ={cq:.2} < 0.8. \
                                 Refusing until coverage improves."
                            )
                        },
                        kbdd_covered: Some(false),
                        cq_score: Some(cq),
                    }
                }
                RiskTier::Educational => GateResult {
                    allowed: true,
                    reason: format!(
                        "T2 educational: not covered but allowed (CQ={cq:.2}). \
                         Response may include caveats."
                    ),
                    kbdd_covered: Some(false),
                    cq_score: Some(cq),
                },
                RiskTier::Creative => unreachable!("handled above"),
            }
        } else {
            // No KBDD checker — fall back to risk tier only
            match self.risk_tier {
                RiskTier::SafetyCritical => GateResult {
                    allowed: false,
                    reason: "T0 safety-critical: no KBDD coverage checker available. \
                             Refusing by default."
                        .into(),
                    kbdd_covered: None,
                    cq_score: None,
                },
                _ => GateResult {
                    allowed: true,
                    reason: format!(
                        "{}: no KBDD checker, allowing by default",
                        self.risk_tier.label()
                    ),
                    kbdd_covered: None,
                    cq_score: None,
                },
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_creative_always_allowed() {
        let gate = HallucinationGate::for_domain("creative");
        let result = gate.check("Tell me about Zorblaxia");
        assert!(result.allowed);
        assert!(result.reason.contains("creative"));
    }

    #[test]
    fn test_safety_critical_no_kbdd_refused() {
        let gate = HallucinationGate::for_domain("medical");
        let result = gate.check("What is Zorblaxia Syndrome?");
        assert!(!result.allowed);
        assert!(result.reason.contains("T0"));
    }

    #[test]
    fn test_educational_no_kbdd_allowed() {
        let gate = HallucinationGate::for_domain("bscs");
        let result = gate.check("What is the Zorblaxian quantum loop?");
        assert!(result.allowed);
    }

    #[test]
    fn test_safety_critical_covered_allowed() {
        let gate =
            HallucinationGate::for_domain("medical").with_kbdd_coverage(Box::new(|_| (true, 0.92)));
        let result = gate.check("What is diabetes?");
        assert!(result.allowed);
        assert_eq!(result.kbdd_covered, Some(true));
        assert_eq!(result.cq_score, Some(0.92));
    }

    #[test]
    fn test_safety_critical_not_covered_refused() {
        let gate = HallucinationGate::for_domain("medical")
            .with_kbdd_coverage(Box::new(|_| (false, 0.75)));
        let result = gate.check("What is Zorblaxia Syndrome?");
        assert!(!result.allowed);
        assert_eq!(result.kbdd_covered, Some(false));
    }

    #[test]
    fn test_professional_high_cq_allowed() {
        let gate = HallucinationGate::for_domain("architect")
            .with_kbdd_coverage(Box::new(|_| (false, 0.85)));
        let result = gate.check("How does microservice X work?");
        assert!(result.allowed);
        assert!(result.reason.contains("CQ=0.85"));
    }

    #[test]
    fn test_professional_low_cq_refused() {
        let gate = HallucinationGate::for_domain("architect")
            .with_kbdd_coverage(Box::new(|_| (false, 0.65)));
        let result = gate.check("How does Zorblaxian architecture work?");
        assert!(!result.allowed);
        assert!(result.reason.contains("CQ=0.65"));
    }

    #[test]
    fn test_educational_not_covered_allowed_with_caveat() {
        let gate = HallucinationGate::for_domain("highschool")
            .with_kbdd_coverage(Box::new(|_| (false, 0.50)));
        let result = gate.check("What is the Zorblaxian method?");
        assert!(result.allowed);
        assert!(result.reason.contains("caveats"));
    }
}
