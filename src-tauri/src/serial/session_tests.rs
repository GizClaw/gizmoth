// Included in daemon::tests so these regressions use the actual HTTP/WS
// handlers and bounded worker channels, without opening physical hardware.

#[tokio::test]
async fn retained_socket_survives_missing_device_and_new_worker() {
    let (_directory, daemon) = fixture();
    daemon.authority.enabled.store(true, Ordering::SeqCst);
    let mut port = port(&daemon);
    port.config.protocol = Protocol::Serial;
    let first_rx = mock_worker(&mut port, Protocol::Serial);
    let shared = port.shared.clone();
    daemon.ports.lock().unwrap().push(port);
    let mut first = connect(daemon.clone()).await;
    notice(&mut first, "granted").await;
    assert_eq!(notice(&mut first, "device").await["ready"], true);
    let mut second = connect(daemon.clone()).await;
    notice(&mut second, "queued").await;
    notice(&mut second, "device").await;

    assert_eq!(
        ws_call(
            &mut first,
            "before-loss",
            "send",
            json!({"text":"old","lineEnding":"none"})
        )
        .await["ok"],
        true
    );
    let Command::Send(stale) = first_rx.try_recv().unwrap() else {
        panic!("send")
    };
    daemon
        .with_port("a", |port| {
            Daemon::stop_worker(port, Connection::Waiting);
            port.present = false;
            Ok(())
        })
        .unwrap();
    let lost = notice(&mut first, "device").await;
    assert_eq!(lost["state"], "disconnected");
    assert_eq!(lost["ready"], false);
    assert_eq!(lost["connection"], "waiting");
    assert!(lost["reason"].as_str().is_some());
    assert_eq!(notice(&mut second, "device").await["ready"], false);
    let unavailable = ws_call(
        &mut first,
        "while-offline",
        "send",
        json!({"text":"not buffered"}),
    )
    .await;
    assert_eq!(unavailable["ok"], false);
    assert_eq!(unavailable["error"]["code"], 503);
    assert_ne!(unavailable["state"], "pending");
    assert_eq!(
        ws_call(
            &mut second,
            "still-pending",
            "send",
            json!({"text":"forbidden"})
        )
        .await["state"],
        "pending"
    );
    {
        let ports = daemon.ports.lock().unwrap();
        let view = daemon.view(&ports[0]);
        assert_eq!(view["holder"]["id"], 1);
        assert_eq!(view["holder"]["title"], "Device diagnostics");
        assert_eq!(view["queue"][0]["id"], 2);
        assert_eq!(view["mode"], "piping");
    }

    let second_rx = daemon
        .with_port("a", |port| {
            port.stopping.take().unwrap().join().unwrap();
            port.present = true;
            Ok(mock_worker_epoch(port, Protocol::Serial, 2))
        })
        .unwrap();
    assert_eq!(notice(&mut first, "device").await["ready"], true);
    assert_eq!(notice(&mut second, "device").await["ready"], true);
    let sequence = shared.device_state.lock().unwrap().sequence;
    shared.transport_down(1, Connection::Error, "stale worker error");
    assert!(!shared.transport_ready(1));
    assert_eq!(shared.device_state.lock().unwrap().sequence, sequence);
    let mut output = Vec::new();
    write_packet(&mut output, &shared, stale, &AtomicBool::new(false)).unwrap();
    assert!(
        output.is_empty(),
        "queued writes from the retired worker cannot be replayed"
    );
    assert_eq!(
        ws_call(
            &mut first,
            "after-reconnect",
            "send",
            json!({"text":"new","lineEnding":"none"})
        )
        .await["ok"],
        true
    );
    let Command::Send(packet) = second_rx.try_recv().unwrap() else {
        panic!("send")
    };
    write_packet(&mut output, &shared, packet, &AtomicBool::new(false)).unwrap();
    assert_eq!(output, b"new");
    assert_eq!(shared.seat.lock().unwrap().holder.as_ref().unwrap().id, 1);
    first.close(None).await.unwrap();
    notice(&mut second, "granted").await;
    second.close(None).await.unwrap();
}

#[tokio::test]
async fn rapid_device_loss_and_recovery_are_both_delivered_without_regranting() {
    let (_directory, daemon) = fixture();
    daemon.authority.enabled.store(true, Ordering::SeqCst);
    let mut port = port(&daemon);
    let _rx = mock_worker(&mut port, Protocol::H2loader);
    let shared = port.shared.clone();
    daemon.ports.lock().unwrap().push(port);
    let mut socket = connect(daemon.clone()).await;
    notice(&mut socket, "granted").await;
    notice(&mut socket, "device").await;
    // No await between transitions: a latest-value watch would lose the outage.
    shared.transport_down(1, Connection::Error, "USB reset");
    shared.transport_ready(1);
    let lost = notice(&mut socket, "device").await;
    let recovered = notice(&mut socket, "device").await;
    assert_eq!(lost["state"], "disconnected");
    assert_eq!(lost["reason"], "USB reset");
    assert_eq!(recovered["state"], "connected");
    assert!(recovered["sequence"].as_u64().unwrap() > lost["sequence"].as_u64().unwrap());
    assert_eq!(shared.seat.lock().unwrap().holder.as_ref().unwrap().id, 1);
    shared.transport_change(
        1,
        DeviceState::Reconnecting,
        Connection::Connecting,
        "H2Loader reboot",
    );
    assert_eq!(notice(&mut socket, "device").await["state"], "reconnecting");
    assert!(shared.seat.lock().unwrap().owns(
        1,
        Caller::Gateway {
            local: true,
            generation: 1
        },
        &daemon.authority
    ));
    shared.transport_ready(1);
    assert_eq!(notice(&mut socket, "device").await["ready"], true);
    socket.close(None).await.unwrap();
}

#[tokio::test]
async fn lagging_availability_events_resynchronize_without_releasing_the_socket() {
    let (_directory, daemon) = fixture();
    daemon.authority.enabled.store(true, Ordering::SeqCst);
    let mut port = port(&daemon);
    port.config.protocol = Protocol::Serial;
    let _rx = mock_worker(&mut port, Protocol::Serial);
    let shared = port.shared.clone();
    daemon.ports.lock().unwrap().push(port);
    let mut socket = connect(daemon.clone()).await;
    notice(&mut socket, "granted").await;
    notice(&mut socket, "device").await;
    for index in 0..80 {
        shared.transport_down(1, Connection::Error, &format!("USB reset {index}"));
        shared.transport_ready(1);
    }
    let current = notice(&mut socket, "device").await;
    assert!(current["missedEvents"].as_u64().unwrap() > 0);
    assert_eq!(current["ready"], true);
    assert_eq!(shared.seat.lock().unwrap().holder.as_ref().unwrap().id, 1);
    assert_eq!(
        ws_call(
            &mut socket,
            "after-resync",
            "send",
            json!({"text":"still held"})
        )
        .await["ok"],
        true
    );
    socket.close(None).await.unwrap();
}

#[tokio::test]
async fn offline_holder_can_cancel_work_and_only_its_close_releases_the_lease() {
    let (_directory, daemon) = fixture();
    daemon.authority.enabled.store(true, Ordering::SeqCst);
    let mut port = port(&daemon);
    let rx = mock_worker(&mut port, Protocol::H2loader);
    let shared = port.shared.clone();
    daemon.ports.lock().unwrap().push(port);
    let mut first = connect(daemon.clone()).await;
    notice(&mut first, "granted").await;
    notice(&mut first, "device").await;
    let job = ws_call(&mut first, "start-work", "refresh", json!({})).await;
    assert_eq!(job["ok"], true);
    assert!(matches!(worker_command(&rx).await, Command::Job));
    let cancel = shared
        .live
        .lock()
        .unwrap()
        .job
        .as_ref()
        .unwrap()
        .cancel
        .clone();
    shared.transport_down(1, Connection::Error, "device disappeared during work");
    notice(&mut first, "device").await;
    assert!(!cancel.load(Ordering::SeqCst));
    assert_eq!(
        ws_call(
            &mut first,
            "cancel-offline",
            "cancel",
            json!({"jobId":job["result"]["id"]})
        )
        .await["ok"],
        true
    );
    assert!(cancel.load(Ordering::SeqCst));
    let mut second = connect(daemon.clone()).await;
    notice(&mut second, "queued").await;
    shared.finish_job(1, Err("cancelled".into()));
    assert_eq!(shared.seat.lock().unwrap().holder.as_ref().unwrap().id, 1);
    first.close(None).await.unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if shared.seat.lock().unwrap().holder.is_none() {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(shared.seat.lock().unwrap().queue.len(), 1);
    assert!(!shared.seat.lock().unwrap().ready);
    shared.transport_ready(1);
    notice(&mut second, "granted").await;
    second.close(None).await.unwrap();
}

#[tokio::test]
async fn operator_disconnect_is_id_specific_while_the_device_is_offline() {
    let (_directory, daemon) = fixture();
    daemon.authority.enabled.store(true, Ordering::SeqCst);
    let mut port = port(&daemon);
    port.config.protocol = Protocol::Serial;
    let _rx = mock_worker(&mut port, Protocol::Serial);
    let shared = port.shared.clone();
    daemon.ports.lock().unwrap().push(port);
    let mut holder = connect(daemon.clone()).await;
    notice(&mut holder, "granted").await;
    notice(&mut holder, "device").await;
    let mut waiter = connect(daemon.clone()).await;
    notice(&mut waiter, "queued").await;
    let mut removed = connect(daemon.clone()).await;
    notice(&mut removed, "queued").await;
    daemon
        .with_port("a", |port| {
            Daemon::stop_worker(port, Connection::Waiting);
            Ok(())
        })
        .unwrap();
    notice(&mut holder, "device").await;
    daemon.disconnect_session("a", 3, false).unwrap();
    notice(&mut removed, "rejected").await;
    assert_eq!(shared.seat.lock().unwrap().holder.as_ref().unwrap().id, 1);
    assert_eq!(shared.seat.lock().unwrap().queue.len(), 1);

    let address = test_server(daemon.clone()).await;
    let http = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(3))
        .build()
        .unwrap();
    let response = http
        .post(format!(
            "http://{address}/v1/ports/a/connections/1/disconnect"
        ))
        .bearer_auth(&daemon.token)
        .json(&json!({"pauseAutomatic":true}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    notice(&mut holder, "revoked").await;
    assert!(shared.seat.lock().unwrap().holder.is_none());
    assert_eq!(shared.seat.lock().unwrap().queue.len(), 1);
    assert!(!daemon.ports.lock().unwrap()[0].config.auto_grant);
    let _next_rx = daemon
        .with_port("a", |port| {
            port.stopping.take().unwrap().join().unwrap();
            port.config.auto_grant = true;
            let rx = mock_worker_epoch(port, Protocol::Serial, 2);
            Daemon::grant_next(port);
            Ok(rx)
        })
        .unwrap();
    notice(&mut waiter, "granted").await;
    assert_eq!(
        daemon.disconnect_session("a", 1, true).unwrap_err().0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(shared.seat.lock().unwrap().holder.as_ref().unwrap().id, 2);
    assert!(daemon.ports.lock().unwrap()[0].config.auto_grant);
    waiter.close(None).await.unwrap();
}

#[tokio::test]
async fn offline_waiters_connect_and_missing_task_titles_are_rejected() {
    let (_directory, daemon) = fixture();
    daemon.authority.enabled.store(true, Ordering::SeqCst);
    daemon.ports.lock().unwrap().push(port(&daemon));
    let mut first = connect(daemon.clone()).await;
    notice(&mut first, "queued").await;
    assert_eq!(notice(&mut first, "device").await["ready"], false);
    let mut second = connect(daemon.clone()).await;
    notice(&mut second, "queued").await;
    daemon.disconnect_session("a", 2, false).unwrap();
    notice(&mut second, "rejected").await;
    assert_eq!(
        daemon.ports.lock().unwrap()[0]
            .shared
            .seat
            .lock()
            .unwrap()
            .queue
            .len(),
        1
    );
    let key = daemon.gateway.lock().unwrap().api_key.clone();
    for query in [
        "client=agent&reason=no-title",
        "client=agent&title=%20",
        "client=agent&title=%00",
    ] {
        let address = test_server(daemon.clone()).await;
        let mut request = format!("ws://{address}/v1/ports/a/pipe?{query}")
            .into_client_request()
            .unwrap();
        request
            .headers_mut()
            .insert("Authorization", format!("Bearer {key}").parse().unwrap());
        let stream = tokio::net::TcpStream::connect(address).await.unwrap();
        let result = tokio_tungstenite::client_async(request, stream).await;
        assert!(
            matches!(result, Err(tokio_tungstenite::tungstenite::Error::Http(response)) if response.status() == 400)
        );
    }
    assert_eq!(
        daemon.ports.lock().unwrap()[0]
            .shared
            .seat
            .lock()
            .unwrap()
            .queue
            .len(),
        1
    );
    first.close(None).await.unwrap();
}
