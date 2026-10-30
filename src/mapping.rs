use serde::{Deserialize, Serialize};
use std::{collections::HashSet, fmt, str::FromStr};

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct Mapping {
    pub local: u16,
    pub remote: u16,
}

impl FromStr for Mapping {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        fn port(value: &str) -> Result<u16, String> {
            if value.is_empty() || !value.bytes().all(|c| c.is_ascii_digit()) {
                return Err("ports must be decimal integers in 1..=65535".into());
            }
            value
                .parse::<u16>()
                .ok()
                .filter(|p| *p != 0)
                .ok_or_else(|| "ports must be decimal integers in 1..=65535".into())
        }
        let (local, remote) = value.split_once(':').unwrap_or((value, value));
        Ok(Self {
            local: port(local)?,
            remote: port(remote)?,
        })
    }
}

impl fmt::Display for Mapping {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.local, self.remote)
    }
}

pub fn validate_host(host: &str) -> Result<(), String> {
    if host.is_empty()
        || host.starts_with('-')
        || host.chars().any(|c| c.is_whitespace() || c.is_control())
        || !host
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-._@".contains(c))
    {
        return Err("host must be an SSH alias, hostname, IPv4 address, or user@host; use an alias for IPv6".into());
    }
    if host.split('@').count() > 2 || host.starts_with('@') || host.ends_with('@') {
        return Err("invalid user@host destination".into());
    }
    Ok(())
}

pub fn validate_mappings(mappings: &[Mapping]) -> Result<(), String> {
    let mut ports = HashSet::new();
    for mapping in mappings {
        if mapping.local == 0 || mapping.remote == 0 || !ports.insert(mapping.local) {
            return Err(format!(
                "invalid or duplicate local port: {}",
                mapping.local
            ));
        }
    }
    if mappings.is_empty() {
        return Err("at least one port mapping is required".into());
    }
    Ok(())
}

pub fn validate_name(name: &str) -> Result<(), String> {
    if name.is_empty()
        || name.len() > 64
        || !name
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"-_".contains(&c))
    {
        return Err(
            "name must contain 1 to 64 ASCII letters, digits, hyphens, or underscores".into(),
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ports_and_mappings() {
        assert_eq!(
            "3000".parse::<Mapping>().unwrap(),
            Mapping {
                local: 3000,
                remote: 3000
            }
        );
        assert_eq!(
            "8080:300".parse::<Mapping>().unwrap(),
            Mapping {
                local: 8080,
                remote: 300
            }
        );
        assert!(
            format!("{}:{}", u16::MAX, u16::MAX)
                .parse::<Mapping>()
                .is_ok()
        );
        for invalid in [
            "0", "65536", "-1", "+1", "", ":80", "80:", "1:2:3", "1.0", " 80",
        ] {
            assert!(invalid.parse::<Mapping>().is_err(), "accepted {invalid:?}");
        }
        assert!(validate_mappings(&["3000".parse().unwrap(), "3000:80".parse().unwrap()]).is_err());
        assert!(
            validate_mappings(&["3000:80".parse().unwrap(), "3001:80".parse().unwrap()]).is_ok()
        );
    }

    #[test]
    fn destination_and_name_boundaries() {
        for host in ["devbox", "user@example.com", "127.0.0.1", "my-host.example"] {
            assert!(validate_host(host).is_ok());
        }
        for host in [
            "",
            "-oProxyCommand=bad",
            "a b",
            "a\nb",
            "$(bad)",
            "a;bad",
            "@host",
            "a@",
            "a@b@c",
            "::1",
        ] {
            assert!(validate_host(host).is_err(), "accepted {host:?}");
        }
        assert!(validate_name("web-1_test").is_ok());
        for name in ["", "../web", "a b", "/tmp/test"] {
            assert!(validate_name(name).is_err());
        }
    }
}
