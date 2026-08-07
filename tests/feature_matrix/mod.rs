//! Feature-matrix conformance.
//!
//! [`corpus`] lists one row per regex syntax, each carrying the support level
//! regexr claims for it. The tests re-derive that level from the engine:
//!
//! * [`Support::Yes`] — every probe must pass, so a supported syntax cannot
//!   regress unnoticed.
//! * [`Support::No`] — at least one probe must fail. Implementing the syntax
//!   fails this test, which is the signal to flip the row.
//!
//! Where a syntax already has a dedicated test module, the row names it in
//! [`Feature::covered_by`] and carries no probes, so nothing is asserted twice.

use regexr::Regex;

mod corpus;

/// Whether regexr claims to implement a feature-matrix row.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Support {
    /// Implemented: every probe passes.
    Yes,
    /// Not implemented: the matrix cell is blank.
    No,
}

/// A single probe: a pattern plus the text it must and must not match.
pub struct Probe {
    /// Pattern under test.
    pub pattern: &'static str,
    /// Text the pattern must match, if the probe checks a positive case.
    pub matches: Option<&'static str>,
    /// Text the pattern must not match, if the probe checks a negative case.
    pub rejects: Option<&'static str>,
    /// Exact substring the match must span, when the probe pins it down.
    pub expect: Option<&'static str>,
    /// Probe runs case-insensitively.
    pub icase: bool,
    /// Probe runs in extended (`x`) mode.
    pub xmode: bool,
}

/// One row of the feature matrix.
pub struct Feature {
    /// The syntax the row names, e.g. `\Q…\E`.
    pub syntax: &'static str,
    /// What the syntax does, used to tell same-named rows apart.
    pub desc: &'static str,
    /// The support level we publish for this row.
    pub support: Support,
    /// Test module that already pins this syntax, if one does. Such a row has
    /// no probes: duplicating them here would mean two places to update.
    pub covered_by: Option<&'static str>,
    /// Probes that decide the row, when nothing else already does.
    pub probes: &'static [Probe],
}

/// A titled block of related rows.
pub struct Group {
    /// Section title, e.g. `Quantifiers`.
    pub name: &'static str,
    /// Rows in the section.
    pub features: &'static [Feature],
}

impl Probe {
    /// Applies the probe's mode flags to its pattern.
    fn pattern(&self) -> String {
        let mut flags = String::new();
        if self.icase {
            flags.push('i');
        }
        if self.xmode {
            flags.push('x');
        }
        if flags.is_empty() {
            self.pattern.to_string()
        } else {
            format!("(?{flags}){}", self.pattern)
        }
    }

    /// Runs the probe, returning why it failed if it did.
    fn run(&self) -> Result<(), String> {
        let pattern = self.pattern();
        let re = match Regex::new(&pattern) {
            Ok(re) => re,
            Err(e) => return Err(format!("`{pattern}` failed to compile: {e}")),
        };

        if let Some(text) = self.matches {
            match re.find(text) {
                None => return Err(format!("`{pattern}` did not match {text:?}")),
                Some(m) => {
                    if let Some(expect) = self.expect {
                        if m.as_str() != expect {
                            return Err(format!(
                                "`{pattern}` matched {:?} in {text:?}, expected {expect:?}",
                                m.as_str()
                            ));
                        }
                    }
                }
            }
        }

        if let Some(text) = self.rejects {
            if re.is_match(text) {
                return Err(format!(
                    "`{pattern}` matched {text:?}, which it should reject"
                ));
            }
        }

        Ok(())
    }
}

/// Runs every probe in a feature and reports the first failure.
fn evaluate(feature: &Feature) -> Result<(), String> {
    for probe in feature.probes {
        probe.run()?;
    }
    Ok(())
}

/// Every row we publish as supported must pass all of its probes.
#[test]
fn supported_features_hold() {
    let mut broken = Vec::new();

    for group in corpus::GROUPS {
        for feature in group.features {
            if feature.support != Support::Yes || feature.covered_by.is_some() {
                continue;
            }
            if let Err(reason) = evaluate(feature) {
                broken.push(format!(
                    "  {} / {} ({}): {reason}",
                    group.name, feature.syntax, feature.desc
                ));
            }
        }
    }

    assert!(
        broken.is_empty(),
        "{} feature-matrix rows claim support but no longer work:\n{}",
        broken.len(),
        broken.join("\n")
    );
}

/// Every row we publish as unsupported must still be unsupported.
///
/// A failure here is good news — it means the syntax now works and the row
/// needs updating.
#[test]
fn unsupported_features_stay_declared() {
    let mut implemented = Vec::new();

    for group in corpus::GROUPS {
        for feature in group.features {
            if feature.support != Support::No {
                continue;
            }
            if evaluate(feature).is_ok() {
                implemented.push(format!(
                    "  {} / {} ({})",
                    group.name, feature.syntax, feature.desc
                ));
            }
        }
    }

    assert!(
        implemented.is_empty(),
        "{} feature-matrix rows are declared unsupported but now pass; \
         set them to `Support::Yes`:\n{}",
        implemented.len(),
        implemented.join("\n")
    );
}

/// The corpus itself must be well formed: every row is decided in exactly one
/// place, and every probe asserts something.
#[test]
fn corpus_is_well_formed() {
    for group in corpus::GROUPS {
        assert!(
            !group.features.is_empty(),
            "group `{}` has no rows",
            group.name
        );
        for feature in group.features {
            match feature.covered_by {
                None => assert!(
                    !feature.probes.is_empty(),
                    "`{}` in `{}` has neither probes nor a `covered_by` pointer",
                    feature.syntax,
                    group.name
                ),
                Some(module) => {
                    assert!(
                        feature.probes.is_empty(),
                        "`{}` in `{}` delegates to `{module}` but also carries probes; \
                         one of the two is a duplicate",
                        feature.syntax,
                        group.name
                    );
                    assert_eq!(
                        feature.support,
                        Support::Yes,
                        "`{}` in `{}` delegates to `{module}`, which only makes sense \
                         for a row we claim to support",
                        feature.syntax,
                        group.name
                    );
                }
            }
            for probe in feature.probes {
                assert!(
                    probe.matches.is_some() || probe.rejects.is_some(),
                    "probe `{}` in `{}` asserts nothing",
                    probe.pattern,
                    feature.syntax
                );
                assert!(
                    probe.expect.is_none() || probe.matches.is_some(),
                    "probe `{}` in `{}` pins a match span but has no matching text",
                    probe.pattern,
                    feature.syntax
                );
            }
        }
    }
}
