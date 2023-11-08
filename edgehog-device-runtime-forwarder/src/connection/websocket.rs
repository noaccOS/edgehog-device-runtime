// Copyright 2023 SECO Mind Srl
// SPDX-License-Identifier: Apache-2.0

//! Define the necessary structs and traits to represent a WebSocket connection.

use std::ops::ControlFlow;

use async_trait::async_trait;
use futures::{SinkExt, StreamExt};
use http::Request;
use tokio::select;
use tokio::sync::mpsc::{channel, Receiver, Sender};
use tracing::{debug, instrument, trace};
use tungstenite::{Error as TungError, Message as TungMessage};

use super::{ConnectionError, Transport, TransportBuilder, WriteHandle, WS_CHANNEL_SIZE};
use crate::connections_manager::WsStream;
use crate::messages::{
    Http as ProtoHttp, HttpMessage as ProtoHttpMessage, HttpRequest as ProtoHttpRequest,
    HttpResponse as ProtoHttpResponse, Id, ProtoMessage, WebSocketMessage as ProtoWebSocketMessage,
};

/// Builder for an [`WebSocket`] connection.
#[derive(Debug)]
pub(crate) struct WebSocketBuilder {
    request: Request<()>,
    rx_con: Receiver<ProtoWebSocketMessage>,
}

impl WebSocketBuilder {
    /// Upgrade the HTTP request and build the channel used to send WebSocket messages to device
    /// services (e.g., TTYD).
    pub(crate) fn with_handle(
        http_req: ProtoHttpRequest,
    ) -> Result<(Self, WriteHandle), ConnectionError> {
        let request = http_req.ws_upgrade()?;
        trace!("HTTP request upgraded");

        // this channel that will be used to send data from the manager to the websocket connection
        let (tx_con, rx_con) = channel::<ProtoWebSocketMessage>(WS_CHANNEL_SIZE);

        Ok((Self { request, rx_con }, WriteHandle::Ws(tx_con)))
    }
}

#[async_trait]
impl TransportBuilder for WebSocketBuilder {
    type Connection = WebSocket;

    #[instrument(skip(self, tx_ws))]
    async fn build(
        self,
        id: &Id,
        tx_ws: Sender<ProtoMessage>,
    ) -> Result<Self::Connection, ConnectionError> {
        // establish a WebSocket connection
        let (ws_stream, http_res) = tokio_tungstenite::connect_async(self.request).await?;
        trace!("WebSocket stream for ID {id} created");

        // send a ProtoMessage with the HTTP generated response to the connections manager
        let proto_msg = ProtoMessage::Http(ProtoHttp::new(
            id.clone(),
            ProtoHttpMessage::Response(ProtoHttpResponse::try_from(http_res)?),
        ));

        tx_ws.send(proto_msg).await.map_err(|_| {
            ConnectionError::Channel(
                "error while returning the Http upgrade response to the ConnectionsManager",
            )
        })?;

        Ok(WebSocket::new(ws_stream, self.rx_con))
    }
}

/// WebSocket connection protocol.
#[derive(Debug)]
pub(crate) struct WebSocket {
    ws_stream: WsStream,
    rx_con: Receiver<ProtoWebSocketMessage>,
}

#[async_trait]
impl Transport for WebSocket {
    /// Write/Read to/from a WebSocket.
    ///
    /// Returns a result only when the device receives a message from a WebSocket connection.
    /// If a message needs to be forwarded to the device's WebSocket connection, a recursive
    /// function call will be invoked.
    async fn next(&mut self, id: &Id) -> Result<Option<ProtoMessage>, ConnectionError> {
        match self.select().await {
            // message from internal websocket connection (e.g., with TTYD) to the connections manager
            WsEither::Read(tung_res) => self.handle_ws_read(id.clone(), tung_res).await,
            // message from the connections manager to the internal websocket connection
            WsEither::Write(chan_data) => {
                if let ControlFlow::Break(()) = self.handle_ws_write(chan_data).await? {
                    return Ok(None);
                }
                self.next(id).await
            }
        }
    }
}

impl WebSocket {
    fn new(ws_stream: WsStream, rx_con: Receiver<ProtoWebSocketMessage>) -> Self {
        Self { ws_stream, rx_con }
    }

    /// The device can either receive a message from the WebSocket connection or may need to
    /// forward data to it.
    async fn select(&mut self) -> WsEither {
        select! {
            tung_res = self.ws_stream.next() => WsEither::Read(tung_res),
            chan_data = self.rx_con.recv() => WsEither::Write(chan_data)
        }
    }

    /// Handle the reception of new data from a WebSocket connection.
    #[instrument(skip(self, tung_res))]
    async fn handle_ws_read(
        &mut self,
        id: Id,
        tung_res: Option<Result<TungMessage, TungError>>,
    ) -> Result<Option<ProtoMessage>, ConnectionError> {
        match tung_res {
            // ws stream closed
            None => {
                debug!("ws stream {id} has been closed, exit");
                Ok(None)
            }
            Some(Ok(tung_msg)) => Ok(Some(ProtoMessage::try_from_tung(id, tung_msg)?)),
            Some(Err(err)) => Err(err.into()),
        }
    }

    /// Forward data from the [`ConnectionsManager`](crate::connections_manager::ConnectionsManager)
    /// to the device WebSocket connection.
    #[instrument(skip_all)]
    async fn handle_ws_write(
        &mut self,
        chan_data: Option<ProtoWebSocketMessage>,
    ) -> Result<ControlFlow<()>, ConnectionError> {
        // convert the websocket proto message into a Tung message
        match chan_data {
            None => {
                debug!("channel dropped, closing connection");
                Ok(ControlFlow::Break(()))
            }
            Some(ws_msg) => {
                self.ws_stream.send(ws_msg.into()).await?;
                trace!("message sent to TTYD");
                Ok(ControlFlow::Continue(()))
            }
        }
    }
}

/// Utility enum to avoid having too much code in the [`select`] macro branches.
enum WsEither {
    Read(Option<Result<TungMessage, TungError>>),
    Write(Option<ProtoWebSocketMessage>),
}
