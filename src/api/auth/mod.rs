use std::collections::HashMap;
use std::fmt::Debug;

use async_trait::async_trait;
use futures::sink::{Sink, SinkExt};
use futures::stream;
use rand;

use super::{ClientInfo, PgWireConnectionState, METADATA_DATABASE, METADATA_USER};
use crate::error::{PgWireError, PgWireResult};
use crate::messages::response::{ReadyForQuery, READY_STATUS_IDLE};
use crate::messages::startup::{Authentication, BackendKeyData, ParameterStatus, Startup};
use crate::messages::{PgWireBackendMessage, PgWireFrontendMessage};

// Alternative design: pass PgWireMessage into the trait and allow the
// implementation to track and define state within itself. This allows better
// support for other auth type like sasl.
#[async_trait]
pub trait StartupHandler: Send + Sync {
    /// A generic frontend message callback during startup phase.
    async fn on_startup<C>(
        &self,
        client: &mut C,
        message: &PgWireFrontendMessage,
    ) -> PgWireResult<()>
    where
        C: ClientInfo + Sink<PgWireBackendMessage> + Unpin + Send,
        C::Error: Debug,
        PgWireError: From<<C as Sink<PgWireBackendMessage>>::Error>;
}

pub trait ServerParameterProvider: Send + Sync {
    fn server_parameters<C>(&self, _client: &C) -> Option<HashMap<String, String>>
    where
        C: ClientInfo;
}

struct NoopServerParameterProvider;

impl ServerParameterProvider for NoopServerParameterProvider {
    fn server_parameters<C>(&self, _client: &C) -> Option<HashMap<String, String>>
    where
        C: ClientInfo,
    {
        None
    }
}

#[derive(Debug, new, Getters)]
#[getset(get = "pub")]
pub struct HashedPassword<'a> {
    salt: &'a [u8],
    hashed_password: &'a String,
}

#[derive(Debug)]
pub enum Password<'a> {
    ClearText(&'a String),
    Hashed(HashedPassword<'a>),
}

#[derive(Debug, new, Getters)]
#[getset(get = "pub")]
pub struct LoginInfo<'a> {
    user: Option<&'a String>,
    database: Option<&'a String>,
    host: String,
}

impl<'a> LoginInfo<'a> {
    pub fn from_client_info<C>(client: &'a C) -> LoginInfo<'a>
    where
        C: ClientInfo,
    {
        LoginInfo {
            user: client.metadata().get(METADATA_USER),
            database: client.metadata().get(METADATA_DATABASE),
            host: client.socket_addr().ip().to_string(),
        }
    }
}

#[async_trait]
pub trait PasswordVerifier: Send + Sync {
    async fn verify_password<'a>(
        &self,
        ctx: LoginInfo<'a>,
        pwd: Password<'a>,
    ) -> PgWireResult<bool>;
}

pub fn save_startup_parameters_to_metadata<C>(client: &mut C, startup_message: &Startup)
where
    C: ClientInfo + Sink<PgWireBackendMessage> + Unpin + Send,
    C::Error: Debug,
{
    client.metadata_mut().extend(
        startup_message
            .parameters()
            .iter()
            .map(|(k, v)| (k.to_owned(), v.to_owned())),
    );
}

pub async fn finish_authentication<C, P>(client: &mut C, server_parameter_provider: &P)
where
    C: ClientInfo + Sink<PgWireBackendMessage> + Unpin + Send,
    C::Error: Debug,
    P: ServerParameterProvider,
{
    let mut messages = vec![PgWireBackendMessage::Authentication(Authentication::Ok)];

    if let Some(parameters) = server_parameter_provider.server_parameters(client) {
        for (k, v) in parameters {
            messages.push(PgWireBackendMessage::ParameterStatus(ParameterStatus::new(
                k, v,
            )));
        }
    }

    // TODO: store this backend key
    messages.push(PgWireBackendMessage::BackendKeyData(BackendKeyData::new(
        std::process::id() as i32,
        rand::random::<i32>(),
    )));
    messages.push(PgWireBackendMessage::ReadyForQuery(ReadyForQuery::new(
        READY_STATUS_IDLE,
    )));
    let mut message_stream = stream::iter(messages.into_iter().map(Ok));
    client.send_all(&mut message_stream).await.unwrap();
    client.set_state(PgWireConnectionState::ReadyForQuery);
}

pub mod cleartext;
pub mod md5pass;
pub mod noop;

// TODO: md5, scram-sha-256(sasl)
