// This file is part of Edgehog.
//
// Copyright 2023 SECO Mind Srl
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//   http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.
//
// SPDX-License-Identifier: Apache-2.0

//! Implement the interaction with the [Astarte rust SDK](astarte_device_sdk).
//!
//! Module responsible for handling a connection between a Device and Astarte.

use std::{collections::HashMap, num::TryFromIntError};

use astarte_device_sdk::{types::AstarteType, AstarteAggregate, AstarteError as SdkError};
use displaydoc::Display;
use thiserror::Error;
use tracing::instrument;
use url::{Host, ParseError, Url};

/// Astarte errors.
#[non_exhaustive]
#[derive(Display, Error, Debug)]
pub enum AstarteError {
    /// Error occurring when different fields from those of the mapping are received.
    Sdk(#[from] SdkError),

    /// Missing url information, `{0}`.
    MissingUrlInfo(&'static str),

    /// Error while parsing an url, `{0}`.
    ParseUrl(#[from] ParseError),

    /// Received a malformed port number, `{0}`.
    ParsePort(#[from] TryFromIntError),
}

/// Struct representing the fields of an aggregated object the Astarte server can send to the device.
#[derive(Debug, Clone)]
pub struct ConnectionInfo {
    /// Hostname or IP address.
    pub host: Host,
    /// Port number.
    pub port: u16,
    session_token: String,
}

impl AstarteAggregate for ConnectionInfo {
    fn astarte_aggregate(self) -> Result<HashMap<String, AstarteType>, SdkError> {
        let mut hm = HashMap::new();
        hm.insert("host".to_string(), self.host.to_string().into());
        hm.insert("port".to_string(), AstarteType::Integer(self.port.into()));
        hm.insert("session_token".to_string(), self.session_token.into());
        Ok(hm)
    }
}

impl TryFrom<&ConnectionInfo> for Url {
    type Error = AstarteError;

    fn try_from(value: &ConnectionInfo) -> Result<Self, Self::Error> {
        if value.session_token.is_empty() {
            return Err(AstarteError::MissingUrlInfo("session token"));
        }

        Url::parse_with_params(
            &format!("ws://{}:{}/device/websocket", value.host, value.port),
            &[("session_token", &value.session_token)],
        )
        .map_err(AstarteError::ParseUrl)
    }
}

/// Parse an `HashMap` containing pairs (Endpoint, [`AstarteType`]) into an URL.
#[instrument(skip_all)]
pub fn retrieve_connection_info(
    mut map: HashMap<String, AstarteType>,
) -> Result<ConnectionInfo, AstarteError> {
    let host = map
        .remove("host")
        .ok_or_else(|| AstarteError::MissingUrlInfo("Missing host (IP or domain name)"))
        .and_then(|t| t.try_into().map_err(AstarteError::from))
        .and_then(|host: String| Host::parse(&host).map_err(AstarteError::from))?;

    let port: u16 = map
        .remove("port")
        .ok_or_else(|| AstarteError::MissingUrlInfo("Missing port value"))
        .and_then(|t| t.try_into().map_err(AstarteError::from))
        .and_then(|port: i32| port.try_into().map_err(AstarteError::from))?;

    let session_token: String = map
        .remove("session_token")
        .ok_or_else(|| AstarteError::MissingUrlInfo("Missing session_token"))?
        .try_into()?;

    Ok(ConnectionInfo {
        host,
        port,
        session_token,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    fn create_cinfo(token: &str) -> ConnectionInfo {
        ConnectionInfo {
            host: Host::Ipv4(Ipv4Addr::LOCALHOST),
            port: 8080,
            session_token: token.to_string(),
        }
    }

    fn create_astarte_hashmap(
        host: &str,
        port: i32,
        session_token: &str,
    ) -> HashMap<String, AstarteType> {
        let mut hm = HashMap::new();

        if !host.is_empty() {
            hm.insert("host".to_string(), AstarteType::String(host.to_string()));
        }
        if port.is_positive() {
            hm.insert("port".to_string(), AstarteType::Integer(port));
        }

        if !session_token.is_empty() {
            hm.insert(
                "session_token".to_string(),
                AstarteType::String(session_token.to_string()),
            );
        }

        hm
    }

    #[test]
    fn test_astarte_aggregate() {
        let cinfo = create_cinfo("test_token");

        let expected = [
            ("host", AstarteType::String("127.0.0.1".to_string())),
            ("port", AstarteType::Integer(8080)),
            (
                "session_token",
                AstarteType::String("test_token".to_string()),
            ),
        ];

        let res = cinfo.astarte_aggregate();

        assert!(res.is_ok());

        let res = res.unwrap();

        for (key, exp_val) in expected {
            assert_eq!(*res.get(key).unwrap(), exp_val);
        }
    }

    #[test]
    fn test_try_from_cinfo() {
        // empty session token generates error
        let cinfo = create_cinfo("");

        assert!(Url::try_from(&cinfo).is_err());

        // ok
        let cinfo = create_cinfo("test_token");

        let case = Url::try_from(&cinfo).unwrap();

        assert_eq!(case.host(), Some(Host::Ipv4(Ipv4Addr::LOCALHOST)));
        assert_eq!(case.port(), Some(8080));
        assert_eq!(case.query(), Some("session_token=test_token"));
    }

    #[test]
    fn test_retrieve_cinfo() {
        let err_cases = [
            create_astarte_hashmap("", 8080, "test_token"),
            create_astarte_hashmap("127.0.0.1", 0, "test_token"),
            create_astarte_hashmap("127.0.0.1", 8080, ""),
        ];

        for hm in err_cases {
            assert!(retrieve_connection_info(hm).is_err());
        }

        let hm = create_astarte_hashmap("127.0.0.1", 8080, "test_token");
        let cinfo = retrieve_connection_info(hm).unwrap();

        assert_eq!(cinfo.host, Host::<&str>::Ipv4(Ipv4Addr::LOCALHOST));
        assert_eq!(cinfo.port, 8080);
        assert_eq!(cinfo.session_token, "test_token".to_string());
    }
}
