use std::{cell::RefCell, collections::HashMap, iter, net::SocketAddr};

use anyhow::{bail, Context};
use clap::Parser;
use futures_util::{SinkExt, StreamExt};
use tokio::{
  net::{TcpListener, TcpStream},
  sync::{broadcast, mpsc, oneshot},
  task,
};
use tokio_tungstenite::{accept_async, tungstenite::Message};
use tracing::{info, warn};
use uuid::Uuid;
use wm_common::{
  AppCommand, AppMetadataData, BindingModesData, ClientResponseData,
  ClientResponseMessage, CommandData, EventSubscribeData,
  EventSubscriptionMessage, FocusedData, MonitorsData, QueryCommand,
  ServerMessage, SubscribableEvent, TilingDirectionData, WindowsData,
  WmEvent, WorkspacesData, DEFAULT_IPC_PORT,
};

use crate::{
  traits::{CommonGetters, TilingDirectionGetters},
  user_config::UserConfig,
  wm::WindowManager,
};

pub struct IpcServer {
  server_task: Option<task::JoinHandle<()>>,
  shutdown_tx: Option<oneshot::Sender<()>>,
  pub message_rx:
    mpsc::Receiver<(String, mpsc::Sender<Message>, broadcast::Sender<()>)>,
  _event_rx: broadcast::Receiver<(SubscribableEvent, WmEvent)>,
  event_tx: broadcast::Sender<(SubscribableEvent, WmEvent)>,
  subscriptions: RefCell<HashMap<Uuid, task::JoinHandle<()>>>,
}

impl IpcServer {
  pub async fn start() -> anyhow::Result<Self> {
    let server_addr = format!("127.0.0.1:{DEFAULT_IPC_PORT}");
    let server = TcpListener::bind(&server_addr).await?;
    info!("IPC server started on: '{}'.", server_addr);
    Ok(Self::with_listener(server))
  }

  fn with_listener(server: TcpListener) -> Self {
    let (message_tx, message_rx) = mpsc::channel(256);
    let (event_tx, _event_rx) = broadcast::channel(16);
    let (shutdown_tx, mut shutdown_rx) = oneshot::channel();

    let task = task::spawn(async move {
      // Dropping this set aborts all connections, including incomplete
      // handshakes. Reap completed tasks instead of retaining their
      // results.
      let mut connections = task::JoinSet::new();
      loop {
        tokio::select! {
          _ = &mut shutdown_rx => {
            connections.shutdown().await;
            break;
          }
          result = server.accept() => {
            let Ok((stream, addr)) = result else { break };
            let message_tx = message_tx.clone();
            connections.spawn(async move {
              if let Err(err) = Self::handle_connection(stream, addr, message_tx).await {
                warn!("Error handling connection: {}", err);
              }
            });
          }
          _ = connections.join_next(), if !connections.is_empty() => {}
        }
      }
    });

    Self {
      server_task: Some(task),
      shutdown_tx: Some(shutdown_tx),
      #[allow(clippy::used_underscore_binding)]
      _event_rx,
      event_tx,
      message_rx,
      subscriptions: RefCell::new(HashMap::new()),
    }
  }

  async fn handle_connection(
    stream: TcpStream,
    addr: SocketAddr,
    message_tx: mpsc::Sender<(
      String,
      mpsc::Sender<Message>,
      broadcast::Sender<()>,
    )>,
  ) -> anyhow::Result<()> {
    info!("Incoming IPC connection from: {}.", addr);

    let ws_stream = accept_async(stream)
      .await
      .context("Error during websocket handshake.")?;

    let (mut outgoing, mut incoming) = ws_stream.split();
    let (response_tx, mut response_rx) = mpsc::channel(64);
    let (disconnection_tx, _) = broadcast::channel(16);
    let mut disconnection_rx = disconnection_tx.subscribe();

    let res = async {
      loop {
        tokio::select! {
          _ = disconnection_rx.recv() => break Ok(()),
          Some(response) = response_rx.recv() => {
            tokio::time::timeout(std::time::Duration::from_secs(5), outgoing.send(response)).await??;
          }
          message = incoming.next() => {
            match message {
              Some(Ok(message)) => {
                if message.is_text() || message.is_binary() {
                  message_tx.try_send((
                    message.to_text()?.to_string(),
                    response_tx.clone(),
                    disconnection_tx.clone(),
                  ))?;
                }
              }
              Some(Err(err)) => bail!("WebSocket error: {}", err),
              None => {
                // WebSocket connection closed.
                break Ok(());
              },
            }
          }
        }
      }
    }
    .await;

    info!("IPC disconnection from: {}.", addr);

    if let Err(err) = disconnection_tx.send(()) {
      warn!("Failed to broadcast disconnection: {}", err);
    }

    res
  }

  pub fn process_message(
    &self,
    message: String,
    response_tx: &mpsc::Sender<Message>,
    disconnection_tx: &broadcast::Sender<()>,
    wm: &mut WindowManager,
    config: &mut UserConfig,
  ) -> anyhow::Result<()> {
    let app_command = AppCommand::try_parse_from(
      iter::once("").chain(message.split_whitespace()),
    );

    let response_data =
      app_command
        .map_err(anyhow::Error::msg)
        .and_then(|app_command| {
          self.handle_app_command(
            app_command,
            response_tx,
            disconnection_tx,
            wm,
            config,
          )
        });

    // Respond to the client with the result of the command.
    response_tx
      .try_send(Self::to_client_response_msg(message, response_data)?)
      .map_err(|err| {
        let _ = disconnection_tx.send(());
        anyhow::anyhow!("Failed to send response: {}", err)
      })?;

    Ok(())
  }

  #[allow(clippy::too_many_lines)]
  fn handle_app_command(
    &self,
    app_command: AppCommand,
    response_tx: &mpsc::Sender<Message>,
    disconnection_tx: &broadcast::Sender<()>,
    wm: &mut WindowManager,
    config: &mut UserConfig,
  ) -> anyhow::Result<ClientResponseData> {
    let response_data = match app_command {
      AppCommand::Query { command } => match command {
        QueryCommand::Windows => {
          ClientResponseData::Windows(WindowsData {
            windows: wm
              .state
              .windows()
              .into_iter()
              .map(|window| window.to_dto())
              .try_collect()?,
          })
        }
        QueryCommand::Workspaces => {
          ClientResponseData::Workspaces(WorkspacesData {
            workspaces: wm
              .state
              .workspaces()
              .into_iter()
              .map(|workspace| workspace.to_dto())
              .try_collect()?,
          })
        }
        QueryCommand::Monitors => {
          ClientResponseData::Monitors(MonitorsData {
            monitors: wm
              .state
              .monitors()
              .into_iter()
              .map(|monitor| monitor.to_dto())
              .try_collect()?,
          })
        }
        QueryCommand::BindingModes => {
          ClientResponseData::BindingModes(BindingModesData {
            binding_modes: wm.state.binding_modes.clone(),
          })
        }
        QueryCommand::Focused => {
          let focused_container = wm
            .state
            .focused_container()
            .context("No focused container.")?;

          ClientResponseData::Focused(FocusedData {
            focused: focused_container.to_dto()?,
          })
        }
        QueryCommand::AppMetadata => {
          ClientResponseData::AppMetadata(AppMetadataData {
            version: env!("VERSION_NUMBER").to_string(),
          })
        }
        QueryCommand::TilingDirection => {
          let direction_container = wm
            .state
            .focused_container()
            .and_then(|focused| focused.direction_container())
            .context("No direction container.")?;

          ClientResponseData::TilingDirection(TilingDirectionData {
            direction_container: direction_container.to_dto()?,
            tiling_direction: direction_container.tiling_direction(),
          })
        }
        QueryCommand::Paused => {
          ClientResponseData::Paused(wm.state.is_paused)
        }
      },
      AppCommand::Command {
        subject_container_id,
        command,
      } => {
        let subject_container_id = wm.process_commands(
          &vec![command],
          subject_container_id,
          config,
        )?;

        ClientResponseData::Command(CommandData {
          subject_container_id,
        })
      }
      AppCommand::Sub { events } => {
        let subscription_id = Uuid::new_v4();
        info!("New event subscription {}: {:?}", subscription_id, events);

        let response_tx = response_tx.clone();
        let mut event_rx = self.event_tx.subscribe();
        let mut disconnection_rx = disconnection_tx.subscribe();
        let disconnect = disconnection_tx.clone();

        let subscription = task::spawn(async move {
          loop {
            tokio::select! {
              // closed() also catches disconnects before this task was
              // subscribed, and connections aborted during server shutdown.
              () = response_tx.closed() => break,
              _ = disconnection_rx.recv() => break,
              result = event_rx.recv() => {
                let (event_type, event) = match result {
                  Ok(event) => event,
                  Err(broadcast::error::RecvError::Lagged(_)) => continue,
                  Err(broadcast::error::RecvError::Closed) => break,
                };
                // Check whether the event is one of the subscribed events.
                if events.contains(&event_type)
                  || events.contains(&SubscribableEvent::All)
                {
                  let send_result = Self::to_event_subscription_msg(
                    subscription_id,
                    event,
                  )
                  .and_then(|event_msg| {
                    response_tx
                      .try_send(event_msg)
                      .map_err(anyhow::Error::from)
                  });

                  if let Err(err) = send_result {
                    warn!("Error emitting WM event: {}", err);
                    let _ = disconnect.send(());
                    break;
                  }
                }
              }
            }
          }
        });
        let mut subscriptions = self.subscriptions.borrow_mut();
        subscriptions.retain(|_, task| !task.is_finished());
        subscriptions.insert(subscription_id, subscription);

        ClientResponseData::EventSubscribe(EventSubscribeData {
          subscription_id,
        })
      }
      AppCommand::Unsub { subscription_id } => {
        if let Some(subscription) =
          self.subscriptions.borrow_mut().remove(&subscription_id)
        {
          subscription.abort();
        }

        ClientResponseData::EventUnsubscribe
      }
      AppCommand::Start { .. } => bail!("Unsupported IPC command."),
    };

    Ok(response_data)
  }

  fn to_client_response_msg(
    client_message: String,
    response_data: anyhow::Result<ClientResponseData>,
  ) -> anyhow::Result<Message> {
    let error = response_data.as_ref().err().map(ToString::to_string);
    let success = response_data.as_ref().is_ok();

    let message = ServerMessage::ClientResponse(ClientResponseMessage {
      client_message,
      data: response_data.ok(),
      error,
      success,
    });

    let message_json = serde_json::to_string(&message)?;
    Ok(Message::Text(message_json.into()))
  }

  fn to_event_subscription_msg(
    subscription_id: Uuid,
    event: WmEvent,
  ) -> anyhow::Result<Message> {
    let message =
      ServerMessage::EventSubscription(EventSubscriptionMessage {
        data: Some(event),
        error: None,
        subscription_id,
        success: true,
      });

    let message_json = serde_json::to_string(&message)?;
    Ok(Message::Text(message_json.into()))
  }

  pub fn process_event(&mut self, event: WmEvent) -> anyhow::Result<()> {
    let event_type = match event {
      WmEvent::ApplicationExiting => SubscribableEvent::ApplicationExiting,
      WmEvent::BindingModesChanged { .. } => {
        SubscribableEvent::BindingModesChanged
      }
      WmEvent::FocusChanged { .. } => SubscribableEvent::FocusChanged,
      WmEvent::FocusedContainerMoved { .. } => {
        SubscribableEvent::FocusedContainerMoved
      }
      WmEvent::MonitorAdded { .. } => SubscribableEvent::MonitorAdded,
      WmEvent::MonitorUpdated { .. } => SubscribableEvent::MonitorUpdated,
      WmEvent::MonitorRemoved { .. } => SubscribableEvent::MonitorRemoved,
      WmEvent::TilingDirectionChanged { .. } => {
        SubscribableEvent::TilingDirectionChanged
      }
      WmEvent::UserConfigChanged { .. } => {
        SubscribableEvent::UserConfigChanged
      }
      WmEvent::WindowManaged { .. } => SubscribableEvent::WindowManaged,
      WmEvent::WindowUnmanaged { .. } => {
        SubscribableEvent::WindowUnmanaged
      }
      WmEvent::WorkspaceActivated { .. } => {
        SubscribableEvent::WorkspaceActivated
      }
      WmEvent::WorkspaceDeactivated { .. } => {
        SubscribableEvent::WorkspaceDeactivated
      }
      WmEvent::WorkspaceUpdated { .. } => {
        SubscribableEvent::WorkspaceUpdated
      }
      WmEvent::PauseChanged { .. } => SubscribableEvent::PauseChanged,
    };

    self
      .event_tx
      .send((event_type, event))
      .map_err(|err| anyhow::anyhow!("Failed to send event: {}", err))?;

    Ok(())
  }

  pub async fn stop(&mut self) {
    info!("Shutting down IPC server.");
    self.message_rx.close();
    if let Some(shutdown_tx) = self.shutdown_tx.take() {
      let _ = shutdown_tx.send(());
    }
    if let Some(task) = self.server_task.take() {
      let _ = task.await;
    }
    let subscriptions: Vec<_> = self
      .subscriptions
      .get_mut()
      .drain()
      .map(|(_, task)| task)
      .collect();
    for task in &subscriptions {
      task.abort();
    }
    for task in subscriptions {
      let _ = task.await;
    }
    while self.message_rx.try_recv().is_ok() {}
  }
}

impl Drop for IpcServer {
  fn drop(&mut self) {
    if let Some(task) = &self.server_task {
      task.abort();
    }
    for (_, task) in self.subscriptions.get_mut().drain() {
      task.abort();
    }
  }
}

#[cfg(test)]
mod tests {
  use std::time::Duration;

  use tokio::{io::AsyncReadExt, time::timeout};

  use super::*;

  fn mock_wm() -> (WindowManager, UserConfig) {
    let (event_tx, event_rx) = mpsc::unbounded_channel();
    let (exit_tx, exit_rx) = mpsc::unbounded_channel();
    let (tick_tx, animation_tick_rx) = mpsc::unbounded_channel();
    let wm = WindowManager {
      event_rx,
      exit_rx,
      animation_tick_rx,
      state: crate::wm_state::WmState::new(
        wm_platform::Dispatcher::mock(),
        event_tx,
        exit_tx,
        tick_tx,
      ),
    };
    let config = UserConfig::new(Some(
      std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../resources/assets/sample-config.yaml"),
    ))
    .unwrap();
    (wm, config)
  }

  async fn server() -> (IpcServer, SocketAddr) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    (IpcServer::with_listener(listener), addr)
  }

  async fn wait_for_subscriptions_to_finish(server: &IpcServer) {
    timeout(Duration::from_secs(2), async {
      while server.event_tx.receiver_count() != 1 {
        task::yield_now().await;
      }
    })
    .await
    .expect("Subscription task leaked");
  }

  #[tokio::test]
  async fn disconnected_client_before_subscribe_does_not_leak() {
    let (mut server, _) = server().await;
    let (mut wm, mut config) = mock_wm();
    let (tx, rx) = mpsc::channel(1);
    let (disconnect, _) = broadcast::channel(16);
    drop(rx);
    for _ in 0..100 {
      server
        .handle_app_command(
          AppCommand::Sub {
            events: vec![SubscribableEvent::All],
          },
          &tx,
          &disconnect,
          &mut wm,
          &mut config,
        )
        .unwrap();
      wait_for_subscriptions_to_finish(&server).await;
    }
    assert!(server.subscriptions.borrow().len() <= 1);
    server.stop().await;
    assert!(server.subscriptions.borrow().is_empty());
  }

  #[tokio::test]
  async fn unsubscribe_burst_cannot_lose_cancellation() {
    let (mut server, _) = server().await;
    let (mut wm, mut config) = mock_wm();
    let (tx, _rx) = mpsc::channel(1);
    let (disconnect, _) = broadcast::channel(16);
    let mut ids = Vec::new();
    for _ in 0..100 {
      let data = server
        .handle_app_command(
          AppCommand::Sub {
            events: vec![SubscribableEvent::All],
          },
          &tx,
          &disconnect,
          &mut wm,
          &mut config,
        )
        .unwrap();
      let ClientResponseData::EventSubscribe(data) = data else {
        panic!("Missing subscription")
      };
      ids.push(data.subscription_id);
    }
    for subscription_id in ids {
      server
        .handle_app_command(
          AppCommand::Unsub { subscription_id },
          &tx,
          &disconnect,
          &mut wm,
          &mut config,
        )
        .unwrap();
    }
    wait_for_subscriptions_to_finish(&server).await;
    assert!(server.subscriptions.borrow().is_empty());
    server.stop().await;
  }

  #[tokio::test]
  async fn slow_subscriber_is_disconnected_instead_of_growing_queue() {
    let (mut server, _) = server().await;
    let (mut wm, mut config) = mock_wm();
    let (tx, mut rx) = mpsc::channel(1);
    let (disconnect, mut disconnected) = broadcast::channel(16);
    server
      .handle_app_command(
        AppCommand::Sub {
          events: vec![SubscribableEvent::All],
        },
        &tx,
        &disconnect,
        &mut wm,
        &mut config,
      )
      .unwrap();
    server.process_event(WmEvent::ApplicationExiting).unwrap();
    server.process_event(WmEvent::ApplicationExiting).unwrap();
    timeout(Duration::from_secs(2), disconnected.recv())
      .await
      .unwrap()
      .unwrap();
    wait_for_subscriptions_to_finish(&server).await;
    assert!(rx.try_recv().is_ok());
    assert!(rx.try_recv().is_err());
    server.stop().await;
  }

  #[tokio::test]
  async fn shutdown_closes_connections_and_incomplete_handshakes() {
    let (mut server, addr) = server().await;
    let (mut ws, _) =
      tokio_tungstenite::connect_async(format!("ws://{addr}"))
        .await
        .unwrap();
    let mut pending = TcpStream::connect(addr).await.unwrap();
    // Let the listener accept the second socket without a WS handshake.
    task::yield_now().await;
    timeout(Duration::from_secs(2), server.stop())
      .await
      .unwrap();
    let end = timeout(Duration::from_secs(2), ws.next()).await.unwrap();
    assert!(end.is_none() || end.unwrap().is_err());
    let mut byte = [0];
    let end = timeout(Duration::from_secs(2), pending.read(&mut byte))
      .await
      .unwrap();
    assert!(matches!(end, Ok(0) | Err(_)));
    assert!(TcpStream::connect(addr).await.is_err());
  }
}
