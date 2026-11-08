//! The `[egress]` table: the hosts a machine's guest may reach, and those it may never reach.

use crate::{Error, Result};
use serde::{Deserialize, Deserializer, Serialize};
use std::fmt;
use std::net::IpAddr;

/// Separates the labels of a hostname.
const LABEL_SEPARATOR: char = '.';

/// Starts a port. An IPv6 literal holds it too, so it refuses a pattern only once the IP
/// literals are taken.
const PORT_SEPARATOR: char = ':';

/// Alone, any host. As a leading `*.`, one or more labels. Elsewhere, exactly one label.
const WILDCARD: &str = "*";

// The reasons `Error::InvalidEgressPattern` gives, after the pattern it quotes.
const EMPTY: &str = "a pattern cannot be empty";
const PORT: &str = "a pattern names a host alone, without a port";
const WILDCARD_LEADING: &str = "a leading '*' must be followed by '.' and a domain";
const WILDCARD_TWICE: &str = "'*.' cannot be combined with another '*'";
const WILDCARD_LABEL: &str = "'*' must occupy a whole label";
const LABEL: &str = "labels contain letters, digits and '-', and cannot be empty";
const IDNA: &str = "the name is not a valid hostname";
const DENY_ANY: &str = "a bare '*' cannot be denied; an empty allow already allows nothing";

/// The `[egress]` table: the hosts a machine's guest may reach, and those it may never reach.
///
/// A missing or empty `allow` allows nothing: every flow is asked, and denied until the approver
/// lands. A bare `*` in `allow` allows every host, whatever else the list holds. A flow meets
/// [`Egress::built_in_denies`] and [`Egress::denies`] before [`Egress::allows`].
/// [`Egress::default`] allows nothing; [`Egress::unrestricted`] allows every host.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Egress {
    /// The hosts a machine's guest may reach. Missing or empty, it allows nothing; a bare `*`
    /// allows every host.
    pub allow: Vec<Pattern>,
    /// The hosts a machine's guest may never reach, checked before `allow`. A bare `*` is
    /// refused: an empty `allow` already allows nothing.
    #[serde(deserialize_with = "deny_patterns")]
    pub deny: Vec<Pattern>,
}

impl Egress {
    /// Returns the lists that allow every host and deny none, `allow = ["*"]` and `deny = []`:
    /// what `setup` writes until the approver lands.
    #[must_use]
    pub fn unrestricted() -> Self {
        Self {
            allow: vec![Pattern(WILDCARD.to_string())],
            deny: Vec::new(),
        }
    }

    /// Returns whether an address is denied under every list: a link-local one, in
    /// `169.254.0.0/16` or `fe80::/10`, where cloud metadata services such as `169.254.169.254`
    /// hand out the host's credentials. An IPv4-mapped address counts as its IPv4 one, so
    /// `::ffff:169.254.169.254` is denied too.
    #[must_use]
    pub fn built_in_denies(ip: IpAddr) -> bool {
        match ip.to_canonical() {
            IpAddr::V4(ipv4) => ipv4.is_link_local(),
            IpAddr::V6(ipv6) => ipv6.is_unicast_link_local(),
        }
    }

    /// Returns whether an entry of `deny` matches the subject: an IP literal equal to its
    /// address, or a hostname entry that matches its name, when it has one.
    #[must_use]
    pub fn denies(&self, subject: &Subject) -> bool {
        self.deny.iter().any(|pattern| {
            names_address(pattern, subject.address)
                || subject.name.as_deref().is_some_and(|host| {
                    pattern.address().is_none() && matches(pattern.as_str(), host)
                })
        })
    }

    /// Returns whether an entry of `allow` admits the subject; the caller checks the denies
    /// first. An IP literal equal to the subject's address admits it, with a name or without.
    /// Otherwise, a subject with a name needs a hostname entry that matches the name, and a bare
    /// `*` matches every name; a subject without one needs a bare `*` while `deny` holds no
    /// hostname, since a flow without a name cannot be checked against one.
    #[must_use]
    pub fn allows(&self, subject: &Subject) -> bool {
        if self.has_address(subject.address) {
            return true;
        }

        match &subject.name {
            Some(host) => self
                .allow
                .iter()
                .any(|pattern| pattern.address().is_none() && matches(pattern.as_str(), host)),
            None => self.has_wildcard() && !self.has_name_deny(),
        }
    }

    /// Returns whether an IP-literal entry in `allow` names the address.
    #[must_use]
    pub fn has_address(&self, ip: IpAddr) -> bool {
        self.allow.iter().any(|pattern| names_address(pattern, ip))
    }

    /// Returns whether a bare `*` is in `allow`.
    #[must_use]
    pub fn has_wildcard(&self) -> bool {
        self.allow
            .iter()
            .any(|pattern| pattern.as_str() == WILDCARD)
    }

    /// Returns whether a hostname entry is in `deny`.
    #[must_use]
    pub fn has_name_deny(&self) -> bool {
        self.deny.iter().any(|pattern| pattern.address().is_none())
    }
}

/// One entry of an `[egress]` list: an exact hostname (`api.github.com`); `*.` and a domain,
/// for one or more labels under it (`*.pypi.org`); `*` as a whole label, for exactly one
/// (`jenkins.*.ci.example.com`); a bare `*`, for any host; or an IP literal (`10.0.0.5`,
/// `2001:db8::5`). A pattern names a host alone, without a port.
///
/// [`Pattern::parse`] and serde run the same checks, so every `Pattern` holds a valid pattern,
/// in its canonical form.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Pattern(String);

impl Pattern {
    /// Parses a pattern, after trimming the whitespace around it.
    ///
    /// The pattern is kept in its canonical form: an IP literal as `IpAddr` prints it, so `0::1`
    /// becomes `::1`, and a hostname lowercased, without a trailing dot, with Unicode labels in
    /// Punycode.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidEgressPattern`] if the pattern is empty, holds a port, misplaces a
    /// `*`, has an empty label or a character outside `[a-z0-9-]`, or is a name IDNA refuses.
    pub fn parse(pattern: &str) -> Result<Self> {
        let pattern = pattern.trim();

        if pattern.is_empty() {
            return Err(Error::invalid_egress_pattern(pattern, EMPTY));
        }

        if let Ok(address) = pattern.parse::<IpAddr>() {
            return Ok(Self(address.to_string()));
        }

        if pattern.contains(PORT_SEPARATOR) {
            return Err(Error::invalid_egress_pattern(pattern, PORT));
        }

        if pattern == WILDCARD {
            return Ok(Self(pattern.to_string()));
        }

        let host =
            normalize_host(pattern).ok_or_else(|| Error::invalid_egress_pattern(pattern, IDNA))?;

        if let Some(rest) = host.strip_prefix(WILDCARD) {
            match rest.strip_prefix(LABEL_SEPARATOR) {
                None | Some("") => {
                    return Err(Error::invalid_egress_pattern(pattern, WILDCARD_LEADING));
                }
                Some(domain) if domain.contains(WILDCARD) => {
                    return Err(Error::invalid_egress_pattern(pattern, WILDCARD_TWICE));
                }
                Some(_) => {}
            }
        }

        for label in host.split(LABEL_SEPARATOR) {
            if label == WILDCARD {
                continue;
            }

            if label.contains(WILDCARD) {
                return Err(Error::invalid_egress_pattern(pattern, WILDCARD_LABEL));
            }

            if label.is_empty() || !label.chars().all(is_label_char) {
                return Err(Error::invalid_egress_pattern(pattern, LABEL));
            }
        }

        Ok(Self(host))
    }

    /// Returns the pattern in its canonical form.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Returns the address an IP literal names, or `None` for a hostname or a wildcard.
    #[must_use]
    pub fn address(&self) -> Option<IpAddr> {
        self.0.parse().ok()
    }
}

impl From<Pattern> for String {
    fn from(value: Pattern) -> Self {
        value.as_str().to_string()
    }
}

impl TryFrom<String> for Pattern {
    type Error = Error;

    fn try_from(value: String) -> Result<Self> {
        Self::parse(&value)
    }
}

impl fmt::Display for Pattern {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// What a flow is judged by: its destination address, always, and its bound name, when it has
/// one. A claimed name is normalized, and an address claimed as a name is no name at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Subject {
    /// The flow's destination address.
    pub address: IpAddr,
    /// The flow's bound name, normalized and in its canonical form, when it has one.
    pub name: Option<String>,
}

impl Subject {
    /// Returns a claimed name, normalized for [`Subject::name`], or `None` when the claim is
    /// empty, is an IP literal (including bracketed IPv6), or fails normalization. Normalized IP
    /// literals are refused too.
    #[must_use]
    pub fn normalize_name(claimed: &str) -> Option<String> {
        let literal = claimed
            .strip_prefix('[')
            .and_then(|host| host.strip_suffix(']'))
            .unwrap_or(claimed);
        if literal.parse::<IpAddr>().is_ok() {
            return None;
        }

        let host = normalize_host(claimed)?;
        if host.is_empty() || host.parse::<IpAddr>().is_ok() {
            return None;
        }

        Some(host)
    }
}

/// Matches a canonical hostname against a parsed pattern; an empty label never matches, and
/// IDNA lets one through (`..example.com`).
fn matches(pattern: &str, host: &str) -> bool {
    if host.split(LABEL_SEPARATOR).any(str::is_empty) {
        return false;
    }

    if pattern == WILDCARD {
        return true;
    }

    // A leading `*.` spans one or more labels, keeping the dot as the suffix boundary.
    if let Some(suffix) = pattern.strip_prefix(WILDCARD) {
        return host
            .strip_suffix(suffix)
            .is_some_and(|prefix| !prefix.is_empty());
    }

    let mut labels = host.split(LABEL_SEPARATOR);
    for label in pattern.split(LABEL_SEPARATOR) {
        let Some(host_label) = labels.next() else {
            return false;
        };
        if label != WILDCARD && label != host_label {
            return false;
        }
    }

    labels.next().is_none()
}

/// Returns whether `pattern` is an IP literal for the same host as `ip`, the check `allow` and
/// `deny` share. An IPv4-mapped address counts as its IPv4 one on either side, since the host
/// dials `::ffff:10.0.0.5` as `10.0.0.5`, so `deny = ["10.0.0.5"]` holds against both. A
/// hostname or a bare `*` names no address.
fn names_address(pattern: &Pattern, ip: IpAddr) -> bool {
    pattern
        .address()
        .is_some_and(|entry| entry.to_canonical() == ip.to_canonical())
}

/// Reads `deny` as `allow` is read, then refuses a bare `*`: allowing nothing is an empty
/// `allow`'s job.
fn deny_patterns<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<Pattern>, D::Error> {
    let patterns = Vec::<Pattern>::deserialize(deserializer)?;
    if patterns.iter().any(|pattern| pattern.as_str() == WILDCARD) {
        return Err(serde::de::Error::custom(Error::invalid_egress_pattern(
            WILDCARD, DENY_ANY,
        )));
    }

    Ok(patterns)
}

/// Normalizes a hostname: strips one trailing dot, applies IDNA, which lowercases and encodes
/// Unicode as Punycode, and strips a trailing dot again, since IDNA turns full stops such as
/// `。` into `.`.
///
/// Returns `None` if the name holds a control character, which IDNA lets through, or if IDNA
/// refuses it.
fn normalize_host(host: &str) -> Option<String> {
    let host = host.strip_suffix(LABEL_SEPARATOR).unwrap_or(host);
    if host.chars().any(char::is_control) {
        return None;
    }

    let mut host = idna::domain_to_ascii(host).ok()?;
    if host.ends_with(LABEL_SEPARATOR) {
        host.pop();
    }

    Some(host)
}

/// Matches `[a-z0-9-]`, every character of a label.
fn is_label_char(c: char) -> bool {
    matches!(c, 'a'..='z' | '0'..='9' | '-')
}

#[cfg(test)]
mod tests {
    use super::{Egress, Pattern, Subject};
    use std::net::{IpAddr, Ipv6Addr};

    #[test]
    fn parses_patterns() {
        // The input, its canonical form, and the address it names when it is an IP literal.
        for (input, canonical, address) in [
            ("api.github.com", "api.github.com", None),
            ("Api.GitHub.com.", "api.github.com", None),
            ("*.pypi.org", "*.pypi.org", None),
            ("*", "*", None),
            ("jenkins.*.ci.example.com", "jenkins.*.ci.example.com", None),
            ("10.0.0.5", "10.0.0.5", Some(IpAddr::from([10, 0, 0, 5]))),
            ("::1", "::1", Some(IpAddr::from(Ipv6Addr::LOCALHOST))),
            ("0::1", "::1", Some(IpAddr::from(Ipv6Addr::LOCALHOST))),
            ("bücher.example", "xn--bcher-kva.example", None),
            (" example.com ", "example.com", None),
        ] {
            let pattern = Pattern::parse(input).unwrap();

            assert_eq!(pattern.as_str(), canonical, "{input:?}");
            assert_eq!(pattern.to_string(), canonical, "{input:?}");
            assert_eq!(pattern.address(), address, "{input:?}");
        }
    }

    #[test]
    fn refuses_malformed_patterns() {
        let empty = "a pattern cannot be empty";
        let port = "a pattern names a host alone, without a port";
        let wildcard_leading = "a leading '*' must be followed by '.' and a domain";
        let wildcard_twice = "'*.' cannot be combined with another '*'";
        let wildcard_label = "'*' must occupy a whole label";
        let label = "labels contain letters, digits and '-', and cannot be empty";
        let idna = "the name is not a valid hostname";

        for (pattern, reason) in [
            ("", empty),
            ("example.com:443", port),
            ("*foo", wildcard_leading),
            ("*.", wildcard_leading),
            ("*.a.*.example", wildcard_twice),
            ("fo*o.example", wildcard_label),
            ("a..example", label),
            ("foo bar", label),
            ("a_b.example", label),
            ("a\u{7}.example", idna),
        ] {
            // The error quotes the pattern, then names the rule it breaks.
            assert_eq!(
                Pattern::parse(pattern).unwrap_err().to_string(),
                format!("invalid egress pattern {pattern:?}: {reason}"),
                "{pattern:?}"
            );
        }
    }

    #[test]
    fn serde_runs_the_same_checks() {
        // The configuration and the daemon's requests carry patterns as plain strings. Reading
        // one runs the same checks as `parse`, so an invalid pattern cannot come in.
        let error = serde_json::from_str::<Pattern>(r#""foo bar""#).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("labels contain letters, digits and '-', and cannot be empty"),
            "{error}"
        );

        let egress = toml::from_str::<Egress>(r#"allow = ["api.github.com"]"#).unwrap();
        assert_eq!(
            egress.allow,
            vec![Pattern::parse("api.github.com").unwrap()]
        );
        assert_eq!(egress.deny, []);

        // Written back, a pattern is its canonical string.
        let json = serde_json::to_string(&Pattern::parse("*.pypi.org").unwrap()).unwrap();
        assert_eq!(json, r#""*.pypi.org""#);
    }

    #[test]
    fn missing_and_empty_lists_allow_nothing() {
        // A missing list reads as an empty one: `allow` allows nothing, and `deny` adds nothing
        // to the built-in denies.
        let missing = toml::from_str::<Egress>("").unwrap();
        assert_eq!(missing, Egress::default());
        assert_eq!(missing.allow, []);
        assert_eq!(missing.deny, []);

        let empty = toml::from_str::<Egress>("allow = []\ndeny = []").unwrap();
        assert_eq!(empty, Egress::default());

        // Unrestricted is spelled out: a bare `*`, alone.
        let unrestricted = Egress::unrestricted();
        assert_eq!(unrestricted.allow, vec![Pattern::parse("*").unwrap()]);
        assert_eq!(unrestricted.deny, []);
    }

    #[test]
    fn deny_refuses_a_bare_wildcard() {
        // Allowing nothing is an empty `allow`'s job, so `deny` refuses a bare `*`.
        let deny_any = "a bare '*' cannot be denied; an empty allow already allows nothing";

        let error = toml::from_str::<Egress>(r#"deny = ["*"]"#).unwrap_err();
        assert!(
            error
                .to_string()
                .contains(&format!(r#"invalid egress pattern "*": {deny_any}"#)),
            "{error}"
        );

        let egress = toml::from_str::<Egress>(r#"deny = ["upload.pypi.org", "10.0.0.5"]"#).unwrap();
        assert_eq!(
            egress.deny,
            vec![
                Pattern::parse("upload.pypi.org").unwrap(),
                Pattern::parse("10.0.0.5").unwrap(),
            ]
        );

        // `allow` keeps its bare `*`.
        assert!(toml::from_str::<Egress>(r#"allow = ["*"]"#).is_ok());
    }

    #[test]
    fn rejects_unknown_egress_keys() {
        // A misspelled key fails to load. An ignored `disallow` would let through every host it
        // names.
        for contents in ["disallow = []", "allowed = []"] {
            assert!(
                toml::from_str::<Egress>(contents).is_err(),
                "accepted {contents:?}"
            );
        }
    }

    #[test]
    fn every_form_of_the_grammar_matches_its_hosts() {
        // The allow entry, a host, and whether the entry matches it.
        for (entry, host, matches) in [
            ("*", "a", true),
            ("*", "a.b.c", true),
            ("*.example.com", "a.example.com", true),
            ("*.example.com", "a.b.example.com", true),
            ("*.example.com", "example.com", false),
            ("*.example.com", "aexample.com", false),
            (
                "jenkins.*.ci.example.com",
                "jenkins.prod.ci.example.com",
                true,
            ),
            ("jenkins.*.ci.example.com", "jenkins.ci.example.com", false),
            (
                "jenkins.*.ci.example.com",
                "jenkins.a.b.ci.example.com",
                false,
            ),
            ("api.github.com", "api.github.com", true),
            ("api.github.com", "www.github.com", false),
            // IDNA lets empty labels through; a host with one never matches.
            ("*.example.com", "..example.com", false),
            ("jenkins.*.ci.example.com", "jenkins..ci.example.com", false),
        ] {
            assert_eq!(
                egress(&[entry], &[]).allows(&name(host)),
                matches,
                "{entry} against {host}"
            );
        }

        // A claimed name is normalized before it is matched.
        let claimed = name(&Subject::normalize_name("Api.GitHub.com.").unwrap());
        assert!(egress(&["api.github.com"], &[]).allows(&claimed));

        // An IP entry matches the address it names, and no other. The same address claimed as a
        // name is no name at all.
        assert!(egress(&["10.0.0.5"], &[]).allows(&address("10.0.0.5")));
        assert!(!egress(&["10.0.0.5"], &[]).allows(&address("10.0.0.6")));
        assert_eq!(Subject::normalize_name("10.0.0.5"), None);
    }

    #[test]
    fn a_wildcard_beside_other_entries_stays_unrestricted() {
        // Hostnames beside a bare `*` narrow nothing, for names and for addresses alike.
        let lists = egress(&["*", "api.github.com"], &[]);
        assert!(lists.allows(&name("example.com")));
        assert!(lists.allows(&address("1.2.3.4")));
    }

    #[test]
    fn an_empty_list_allows_nothing() {
        // A missing or empty `allow`: no name and no address get through.
        let nothing = Egress::default();
        assert!(!nothing.allows(&name("example.com")));
        assert!(!nothing.allows(&address("1.2.3.4")));
        assert!(!nothing.has_wildcard());
    }

    #[test]
    fn built_in_denies_hold() {
        // Every link-local address, v4-mapped included, and no other.
        for ip in [
            "169.254.169.254",
            "169.254.10.10",
            "fe80::1",
            "::ffff:169.254.169.254",
        ] {
            assert!(Egress::built_in_denies(ip.parse().unwrap()), "{ip}");
        }
        for ip in ["93.184.216.34", "fd00::1"] {
            assert!(!Egress::built_in_denies(ip.parse().unwrap()), "{ip}");
        }
    }

    #[test]
    fn an_unnamed_flow_needs_an_address_entry_without_a_wildcard() {
        // A hostname entry cannot vouch for a flow that carries no name; an IP entry can.
        assert!(!egress(&["api.github.com"], &[]).allows(&address("1.2.3.4")));
        assert!(egress(&["api.github.com", "1.2.3.4"], &[]).allows(&address("1.2.3.4")));
    }

    #[test]
    fn an_address_entry_admits_a_named_flow() {
        // A name the host binds to a listed address buys no less than the address alone, since the
        // guest could drop the name and reach the address anyway.
        let subject = Subject {
            address: IpAddr::from([10, 0, 0, 5]),
            name: Some("git.corp".to_string()),
        };
        assert!(egress(&["10.0.0.5"], &[]).allows(&subject));
    }

    #[test]
    fn a_name_deny_turns_away_unnamed_flows() {
        // Under a bare `*`, a hostname in `deny` cannot be checked against a flow that carries no
        // name, so that flow is turned away unless an IP entry lists its address.
        let lists = egress(&["*"], &["evil.example"]);
        assert!(lists.denies(&name("evil.example")));
        assert!(!lists.denies(&name("a.example")));
        assert!(lists.allows(&name("a.example")));
        assert!(!lists.allows(&address("1.2.3.4")));
        assert!(lists.has_name_deny());

        let listed = egress(&["*", "1.2.3.4"], &["evil.example"]);
        assert!(listed.allows(&address("1.2.3.4")));
    }

    #[test]
    fn an_address_deny_leaves_unnamed_flows_alone() {
        // An address in `deny` holds against the address of every flow, so a bare `*` keeps
        // admitting the flows that carry no name.
        let lists = egress(&["*"], &["93.184.216.34"]);
        assert!(lists.denies(&address("93.184.216.34")));
        assert!(!lists.denies(&name("example.com")));
        assert!(lists.allows(&address("1.2.3.4")));
        assert!(!lists.has_name_deny());
    }

    #[test]
    fn every_form_of_the_grammar_denies_its_hosts() {
        // A deny entry, and a name it denies: each form of the grammar, then the spellings a guest
        // may present, which normalize to the entry's own.
        for (entry, claimed) in [
            ("evil.example", "evil.example"),
            ("*.evil.example", "test.evil.example"),
            ("*.evil.example", "a.b.evil.example"),
            ("jenkins.*.ci.example.com", "jenkins.prod.ci.example.com"),
            ("evil.example", "EVIL.Example."),
            ("evil.example", "evil。example"),
            ("evil.example", "evil.example。"),
            ("evil.example", "ｅｖｉｌ.example"),
            ("bücher.example", "BÜCHER.example"),
            ("bücher.example", "xn--bcher-kva.example"),
        ] {
            let subject = name(&Subject::normalize_name(claimed).unwrap());
            assert!(
                egress(&[], &[entry]).denies(&subject),
                "{entry} against {claimed:?}"
            );
        }

        // A deny entry, an address, and whether the entry denies it. An IPv4-mapped address is
        // the IPv4 one, either way round; the deprecated IPv4-compatible `::10.0.0.5` is not.
        for (entry, ip, denied) in [
            ("10.0.0.5", "10.0.0.5", true),
            ("10.0.0.5", "10.0.0.6", false),
            ("2001:db8::1", "2001:db8::1", true),
            ("10.0.0.5", "::10.0.0.5", false),
            ("10.0.0.5", "::ffff:10.0.0.5", true),
            ("::ffff:10.0.0.5", "10.0.0.5", true),
        ] {
            assert_eq!(
                egress(&[], &[entry]).denies(&address(ip)),
                denied,
                "{entry} against {ip}"
            );
        }
    }

    #[test]
    fn subjects_normalize_and_refuse_addresses_as_names() {
        // A claimed name is normalized as a pattern is. An address claimed as a name, bracketed
        // or not, is no name, and neither is an empty claim.
        assert_eq!(
            Subject::normalize_name("Example.COM."),
            Some("example.com".to_string())
        );
        assert_eq!(Subject::normalize_name("169.254.169.254"), None);
        assert_eq!(Subject::normalize_name("[::1]"), None);
        assert_eq!(Subject::normalize_name(""), None);
    }

    /// The `[egress]` lists, each entry parsed as a pattern.
    fn egress(allow: &[&str], deny: &[&str]) -> Egress {
        let parse = |entries: &[&str]| -> Vec<Pattern> {
            entries
                .iter()
                .map(|entry| Pattern::parse(entry).unwrap())
                .collect()
        };

        Egress {
            allow: parse(allow),
            deny: parse(deny),
        }
    }

    /// A flow with a bound name, already normalized. Its address, `192.0.2.1`, is one no list
    /// here names, so the name alone decides.
    fn name(host: &str) -> Subject {
        Subject {
            address: IpAddr::from([192, 0, 2, 1]),
            name: Some(host.to_string()),
        }
    }

    /// A flow without a name, judged by its destination address alone.
    fn address(ip: &str) -> Subject {
        Subject {
            address: ip.parse().unwrap(),
            name: None,
        }
    }
}
