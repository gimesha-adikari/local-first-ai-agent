//! Canonical HTTP endpoints. Parsing happens once, when the trusted provider
//! configuration registers a destination; the transport only ever sees the
//! canonical form.

use std::{fmt, net::IpAddr};

use agent_core::capability::Locality;
use serde::Serialize;

use super::address::{AddressClass, NetworkScope, classify};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Scheme {
    Https,
    Http,
}

impl Scheme {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Https => "https",
            Self::Http => "http",
        }
    }

    fn default_port(self) -> u16 {
        match self {
            Self::Https => 443,
            Self::Http => 80,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Host {
    /// Lowercase ASCII (punycode) without a trailing dot.
    Domain(String),
    Ip(IpAddr),
}

impl fmt::Display for Host {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Domain(name) => formatter.write_str(name),
            Self::Ip(IpAddr::V4(ip)) => write!(formatter, "{ip}"),
            Self::Ip(IpAddr::V6(ip)) => write!(formatter, "[{ip}]"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EndpointError {
    Invalid,
    Scheme,
    Credentials,
    QueryOrFragment,
    Host,
    Path,
    /// The address class is never allowed, such as link-local or multicast.
    Address,
    /// Third-party providers need HTTPS on a public address.
    InsecureRemote,
    /// Unencrypted HTTP is only for loopback or a pinned local address.
    InsecureScheme,
    /// A local-network server must be given by its IP address.
    PrivateName,
}

impl fmt::Display for EndpointError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Invalid => "Enter a valid http or https URL.",
            Self::Scheme => {
                "Use an https URL, or http for a server on this computer or your local network."
            }
            Self::Credentials => {
                "Remove the user name or password from the URL. Use the API key field instead."
            }
            Self::QueryOrFragment => "Remove the query or fragment from the URL.",
            Self::Host => "The URL host name is not valid.",
            Self::Path => {
                "Use a simple URL path made of letters, digits, '-', '.', '_', '~' and '/'."
            }
            Self::Address => "This address type cannot be used for a provider.",
            Self::InsecureRemote => "Cloud providers must use https on a public address.",
            Self::InsecureScheme => {
                "Unencrypted http is allowed only for this computer or a local-network IP address."
            }
            Self::PrivateName => "Give a local-network server by its IP address.",
        })
    }
}

/// A canonical scheme, host, port and base path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoint {
    scheme: Scheme,
    host: Host,
    port: u16,
    /// Empty, or starts with '/' and has no trailing '/'.
    base_path: String,
}

const MAX_URL: usize = 512;
const MAX_PATH: usize = 256;

pub(super) fn valid_domain(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 253
        && name.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        })
        // A name made only of digits and dots would be read as an address by
        // some resolvers.
        && !name.bytes().all(|b| b.is_ascii_digit() || b == b'.')
}

impl Endpoint {
    pub fn parse(input: &str) -> Result<Self, EndpointError> {
        let input = input.trim();
        if input.is_empty() || input.len() > MAX_URL || input.chars().any(char::is_control) {
            return Err(EndpointError::Invalid);
        }
        let url = url::Url::parse(input).map_err(|_| EndpointError::Invalid)?;
        let scheme = match url.scheme() {
            "https" => Scheme::Https,
            "http" => Scheme::Http,
            _ => return Err(EndpointError::Scheme),
        };
        if !url.username().is_empty() || url.password().is_some() {
            return Err(EndpointError::Credentials);
        }
        if url.query().is_some() || url.fragment().is_some() {
            return Err(EndpointError::QueryOrFragment);
        }
        let host = match url.host().ok_or(EndpointError::Host)? {
            url::Host::Domain(name) => {
                let name = name.strip_suffix('.').unwrap_or(name).to_ascii_lowercase();
                if !valid_domain(&name) {
                    return Err(EndpointError::Host);
                }
                Host::Domain(name)
            }
            url::Host::Ipv4(ip) => Host::Ip(IpAddr::V4(ip)),
            url::Host::Ipv6(ip) => Host::Ip(ip.to_ipv4_mapped().map_or(IpAddr::V6(ip), IpAddr::V4)),
        };
        let port = url.port().unwrap_or_else(|| scheme.default_port());
        if port == 0 {
            return Err(EndpointError::Invalid);
        }
        let path = url.path().trim_end_matches('/');
        if path.len() > MAX_PATH
            || path.contains("//")
            || !path
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-._~/".contains(&b))
            || path
                .split('/')
                .any(|segment| segment == "." || segment == "..")
        {
            return Err(EndpointError::Path);
        }
        Ok(Self {
            scheme,
            host,
            port,
            base_path: path.to_owned(),
        })
    }

    pub fn scheme(&self) -> Scheme {
        self.scheme
    }

    pub fn host(&self) -> &Host {
        &self.host
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    pub fn base_path(&self) -> &str {
        &self.base_path
    }

    /// `scheme://host:port`, always with the port, for binding credentials.
    pub fn origin(&self) -> String {
        format!("{}://{}:{}", self.scheme.as_str(), self.host, self.port)
    }

    /// The URL form shown to people: default ports omitted.
    pub fn display(&self) -> String {
        if self.port == self.scheme.default_port() {
            format!("{}://{}{}", self.scheme.as_str(), self.host, self.base_path)
        } else {
            format!(
                "{}://{}:{}{}",
                self.scheme.as_str(),
                self.host,
                self.port,
                self.base_path
            )
        }
    }

    /// The Host header value.
    pub(crate) fn authority(&self) -> String {
        if self.port == self.scheme.default_port() {
            self.host.to_string()
        } else {
            format!("{}:{}", self.host, self.port)
        }
    }

    /// The network scope a provider of this locality may use, or why the
    /// combination is refused.
    pub fn scope_for(&self, locality: Locality) -> Result<NetworkScope, EndpointError> {
        let scope = match &self.host {
            Host::Domain(name) if name == "localhost" || name.ends_with(".localhost") => {
                NetworkScope::Loopback
            }
            Host::Domain(name) if name.ends_with(".local") || name.ends_with(".lan") => {
                return Err(EndpointError::PrivateName);
            }
            Host::Domain(_) => NetworkScope::Public,
            Host::Ip(ip) => match classify(*ip) {
                AddressClass::Public => NetworkScope::Public,
                AddressClass::Loopback => NetworkScope::Loopback,
                AddressClass::Private => NetworkScope::Pinned(vec![*ip]),
                _ => return Err(EndpointError::Address),
            },
        };
        match (locality, &scope, self.scheme) {
            (Locality::OnDevice, ..) => Err(EndpointError::Address),
            (Locality::ThirdParty, NetworkScope::Public, Scheme::Https) => Ok(scope),
            (Locality::ThirdParty, ..) => Err(EndpointError::InsecureRemote),
            (_, NetworkScope::Public, Scheme::Http) => Err(EndpointError::InsecureScheme),
            _ => Ok(scope),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn canonical(input: &str) -> String {
        Endpoint::parse(input).unwrap().display()
    }

    #[test]
    fn canonicalizes_equivalent_spellings_to_one_origin() {
        let a = Endpoint::parse("HTTPS://API.Example.COM.:443/v1/").unwrap();
        let b = Endpoint::parse("https://api.example.com/v1").unwrap();
        assert_eq!(a, b);
        assert_eq!(a.origin(), "https://api.example.com:443");
        assert_eq!(a.display(), "https://api.example.com/v1");
        assert_eq!(canonical("http://127.0.0.1:8080/"), "http://127.0.0.1:8080");
        // Alternative IPv4 spellings parse to the same canonical address.
        assert_eq!(
            canonical("http://0x7f.1:8080/v1"),
            "http://127.0.0.1:8080/v1"
        );
        assert_eq!(canonical("http://2130706433:8080"), "http://127.0.0.1:8080");
        // An IPv4-mapped IPv6 literal is the IPv4 address.
        assert_eq!(
            Endpoint::parse("http://[::ffff:10.0.0.5]:8000").unwrap(),
            Endpoint::parse("http://10.0.0.5:8000").unwrap()
        );
        assert_eq!(
            canonical("https://[2001:4860::8888]/v1"),
            "https://[2001:4860::8888]/v1"
        );
        // Dot segments are resolved before the path is accepted.
        assert_eq!(
            canonical("https://api.example.com/a/../v1"),
            "https://api.example.com/v1"
        );
        // Internationalized names become punycode.
        assert_eq!(
            canonical("https://bücher.example/v1"),
            "https://xn--bcher-kva.example/v1"
        );
    }

    #[test]
    fn rejects_ambiguous_or_unsafe_urls() {
        for (input, error) in [
            ("ftp://example.com", EndpointError::Scheme),
            ("file:///etc/passwd", EndpointError::Scheme),
            (
                "https://user:pw@api.example.com",
                EndpointError::Credentials,
            ),
            ("https://key@api.example.com", EndpointError::Credentials),
            (
                "https://api.example.com/v1?key=x",
                EndpointError::QueryOrFragment,
            ),
            (
                "https://api.example.com/v1#x",
                EndpointError::QueryOrFragment,
            ),
            ("https://api.example.com/v%31", EndpointError::Path),
            ("https://api.example.com/a//b", EndpointError::Path),
            ("https://api_example.com", EndpointError::Host),
            ("https://", EndpointError::Invalid),
            ("not a url", EndpointError::Invalid),
            ("https://api.example.com:0", EndpointError::Invalid),
        ] {
            assert_eq!(Endpoint::parse(input), Err(error), "{input}");
        }
    }

    #[test]
    fn scope_rules_bind_locality_scheme_and_address() {
        let scope = |url: &str, locality| Endpoint::parse(url).unwrap().scope_for(locality);
        assert_eq!(
            scope("https://api.example.com/v1", Locality::ThirdParty),
            Ok(NetworkScope::Public)
        );
        assert_eq!(
            scope("http://api.example.com/v1", Locality::ThirdParty),
            Err(EndpointError::InsecureRemote)
        );
        assert_eq!(
            scope("https://127.0.0.1:8443", Locality::ThirdParty),
            Err(EndpointError::InsecureRemote)
        );
        assert_eq!(
            scope("http://localhost:11434/v1", Locality::SelfHosted),
            Ok(NetworkScope::Loopback)
        );
        assert_eq!(
            scope("http://[::1]:8080", Locality::SelfHosted),
            Ok(NetworkScope::Loopback)
        );
        assert_eq!(
            scope("http://192.168.1.20:8000/v1", Locality::SelfHosted),
            Ok(NetworkScope::Pinned(vec!["192.168.1.20".parse().unwrap()]))
        );
        assert_eq!(
            scope("http://gpu.example.com/v1", Locality::SelfHosted),
            Err(EndpointError::InsecureScheme)
        );
        assert_eq!(
            scope("https://gpu.example.com/v1", Locality::SelfHosted),
            Ok(NetworkScope::Public)
        );
        assert_eq!(
            scope("http://gpu-box.local:8000", Locality::SelfHosted),
            Err(EndpointError::PrivateName)
        );
        for blocked in [
            "http://169.254.169.254/latest",
            "http://[fe80::1]:80",
            "http://0.0.0.0:80",
            "http://224.0.0.1",
            "http://255.255.255.255",
            "http://192.0.2.10",
        ] {
            assert_eq!(
                scope(blocked, Locality::SelfHosted),
                Err(EndpointError::Address),
                "{blocked}"
            );
        }
    }
}
