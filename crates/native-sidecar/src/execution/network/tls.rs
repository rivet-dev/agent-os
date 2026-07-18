use super::super::*;

#[derive(Debug)]
struct InsecureTlsVerifier {
    supported_schemes: Vec<SignatureScheme>,
}

impl ServerCertVerifier for InsecureTlsVerifier {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.supported_schemes.clone()
    }
}

fn loopback_tls_transport_registry(
) -> &'static Mutex<BTreeMap<String, Weak<crate::state::LoopbackTlsTransportPair>>> {
    static REGISTRY: OnceLock<
        Mutex<BTreeMap<String, Weak<crate::state::LoopbackTlsTransportPair>>>,
    > = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(BTreeMap::new()))
}

#[cfg(test)]
#[allow(dead_code)]
pub(crate) fn loopback_tls_registry_len() -> usize {
    loopback_tls_transport_registry()
        .lock()
        .expect("loopback TLS transport registry lock poisoned")
        .len()
}

#[cfg(test)]
pub(crate) fn loopback_tls_registry_contains(key: &str) -> bool {
    loopback_tls_transport_registry()
        .lock()
        .expect("loopback TLS transport registry lock poisoned")
        .contains_key(key)
}

fn loopback_tls_transport_key(
    vm_id: &str,
    socket_id: SocketId,
    peer_socket_id: SocketId,
) -> String {
    let (lower, higher) = if socket_id <= peer_socket_id {
        (socket_id, peer_socket_id)
    } else {
        (peer_socket_id, socket_id)
    };
    format!("{vm_id}:{lower}:{higher}")
}

pub(in crate::execution) fn loopback_tls_endpoint(
    vm_id: &str,
    socket_id: SocketId,
    peer_socket_id: SocketId,
    resources: Arc<ResourceLedger>,
) -> Result<crate::state::LoopbackTlsEndpoint, SidecarError> {
    let key = loopback_tls_transport_key(vm_id, socket_id, peer_socket_id);
    let registry = loopback_tls_transport_registry();
    let mut transports = registry.lock().map_err(|_| {
        SidecarError::InvalidState(String::from(
            "loopback TLS transport registry lock poisoned",
        ))
    })?;
    transports.retain(|_, pair| pair.strong_count() > 0);
    let pair = transports
        .get(&key)
        .and_then(Weak::upgrade)
        .unwrap_or_else(|| {
            let pair = Arc::new(crate::state::LoopbackTlsTransportPair {
                state: Mutex::new(crate::state::LoopbackTlsTransportPairState::default()),
                ready: std::sync::Condvar::new(),
                resources,
            });
            transports.insert(key.clone(), Arc::downgrade(&pair));
            pair
        });
    Ok(crate::state::LoopbackTlsEndpoint {
        pair,
        is_lower_socket: socket_id <= peer_socket_id,
        registry_key: Some(key),
    })
}

fn release_loopback_tls_reservations(reservations: &mut VecDeque<Reservation>, mut bytes: usize) {
    while bytes != 0 {
        let Some(front) = reservations.front_mut() else {
            eprintln!(
                "ERR_AGENTOS_TLS_ACCOUNTING_UNDERFLOW: loopback ciphertext has no reservation for {bytes} consumed bytes"
            );
            return;
        };
        if front.amount() <= bytes {
            bytes -= front.amount();
            reservations.pop_front();
        } else {
            let released = front
                .split(bytes)
                .expect("checked loopback TLS reservation split");
            drop(released);
            bytes = 0;
        }
    }
}

impl crate::state::LoopbackTlsEndpoint {
    pub(in crate::execution) fn shutdown_write(&self) -> Result<(), SidecarError> {
        let mut state = self.pair.state.lock().map_err(|_| {
            SidecarError::InvalidState(String::from("loopback TLS transport lock poisoned"))
        })?;
        let peer_waker = if self.is_lower_socket {
            state.lower_write_closed = true;
            state.higher_read_waker.take()
        } else {
            state.higher_write_closed = true;
            state.lower_read_waker.take()
        };
        self.pair.ready.notify_all();
        drop(state);
        if let Some(waker) = peer_waker {
            waker.wake();
        }
        Ok(())
    }

    fn close_endpoint(&self) -> Result<(), SidecarError> {
        let mut state = self.pair.state.lock().map_err(|_| {
            SidecarError::InvalidState(String::from("loopback TLS transport lock poisoned"))
        })?;
        let peer_waker = if self.is_lower_socket {
            state.lower_write_closed = true;
            state.lower_closed = true;
            state.higher_read_waker.take()
        } else {
            state.higher_write_closed = true;
            state.higher_closed = true;
            state.lower_read_waker.take()
        };
        self.pair.ready.notify_all();
        drop(state);
        if let Some(waker) = peer_waker {
            waker.wake();
        }
        Ok(())
    }
}

pub(in crate::execution) fn parse_tls_client_hello_from_bytes(
    buffer: &[u8],
) -> Result<Option<TlsClientHello>, SidecarError> {
    if buffer.is_empty() {
        return Ok(None);
    }

    let mut acceptor = rustls::server::Acceptor::default();
    let mut cursor = Cursor::new(buffer);
    acceptor.read_tls(&mut cursor).map_err(sidecar_net_error)?;
    let Some(accepted) = acceptor.accept().map_err(|(error, _)| {
        SidecarError::Execution(format!("failed to parse TLS client hello: {error}"))
    })?
    else {
        return Ok(None);
    };
    let client_hello = accepted.client_hello();
    let alpn_protocols = client_hello.alpn().map(|protocols| {
        protocols
            .filter_map(|protocol| String::from_utf8(protocol.to_vec()).ok())
            .collect::<Vec<_>>()
    });
    Ok(Some(TlsClientHello {
        servername: client_hello.server_name().map(str::to_owned),
        alpn_protocols,
    }))
}

pub(in crate::execution) fn peek_loopback_tls_client_hello(
    vm_id: &str,
    socket_id: SocketId,
    peer_socket_id: SocketId,
) -> Result<Option<TlsClientHello>, SidecarError> {
    let key = loopback_tls_transport_key(vm_id, socket_id, peer_socket_id);
    let registry = loopback_tls_transport_registry();
    let pair = registry
        .lock()
        .map_err(|_| {
            SidecarError::InvalidState(String::from(
                "loopback TLS transport registry lock poisoned",
            ))
        })?
        .get(&key)
        .and_then(Weak::upgrade);
    let Some(pair) = pair else {
        return Ok(None);
    };
    let is_lower_socket = socket_id <= peer_socket_id;
    let state = pair.state.lock().map_err(|_| {
        SidecarError::InvalidState(String::from("loopback TLS transport lock poisoned"))
    })?;
    let buffered = if is_lower_socket {
        state.higher_to_lower.iter().copied().collect::<Vec<_>>()
    } else {
        state.lower_to_higher.iter().copied().collect::<Vec<_>>()
    };
    drop(state);
    parse_tls_client_hello_from_bytes(&buffered)
}

impl Drop for crate::state::LoopbackTlsEndpoint {
    fn drop(&mut self) {
        if let Err(error) = self.close_endpoint() {
            eprintln!("ERR_AGENTOS_TLS_LOOPBACK_CLOSE: endpoint cleanup failed: {error}");
        }

        // Eagerly prune this endpoint's registry entry once we are the last owner
        // of the shared transport pair. Without this, the `Weak` entry survives
        // until the next `loopback_tls_endpoint()` call runs its lazy `retain()`,
        // so dead entries accumulate under intermittent use. We must NOT remove
        // the entry while a peer endpoint still shares the pair, otherwise a later
        // connection for the same socket pair would fail to find it and build a
        // mismatched fresh pair.
        let Some(key) = self.registry_key.take() else {
            return;
        };
        let Ok(mut transports) = loopback_tls_transport_registry().lock() else {
            // Lock poisoned: leave the entry for the lazy `retain()` to reclaim.
            return;
        };
        let should_remove = match transports.get(&key) {
            // Only prune when the registered entry still points at *our* pair and
            // `self` is its last strong owner. During `Drop` `self.pair` is still
            // alive, so a strong count of 1 (after dropping the temporary upgrade)
            // means no other endpoint references it.
            Some(weak) => match weak.upgrade() {
                Some(existing) => {
                    let same_pair = Arc::ptr_eq(&existing, &self.pair);
                    drop(existing);
                    same_pair && Arc::strong_count(&self.pair) <= 1
                }
                None => true,
            },
            None => false,
        };
        if should_remove {
            transports.remove(&key);
        }
    }
}

impl Read for crate::state::LoopbackTlsEndpoint {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let mut state = self
            .pair
            .state
            .lock()
            .map_err(|_| std::io::Error::other("loopback TLS transport lock poisoned"))?;

        {
            let (peer_write_closed, peer_closed) = if self.is_lower_socket {
                (state.higher_write_closed, state.higher_closed)
            } else {
                (state.lower_write_closed, state.lower_closed)
            };

            let incoming = if self.is_lower_socket {
                &mut state.higher_to_lower
            } else {
                &mut state.lower_to_higher
            };

            if !incoming.is_empty() {
                let count = incoming.len().min(buffer.len());
                let (head, tail) = incoming.as_slices();
                let head_count = head.len().min(count);
                buffer[..head_count].copy_from_slice(&head[..head_count]);
                if head_count < count {
                    buffer[head_count..count].copy_from_slice(&tail[..count - head_count]);
                }
                incoming.drain(..count);
                let writer_waker = if self.is_lower_socket {
                    release_loopback_tls_reservations(
                        &mut state.higher_to_lower_reservations,
                        count,
                    );
                    release_loopback_tls_reservations(
                        &mut state.higher_to_lower_tls_reservations,
                        count,
                    );
                    state.higher_write_waker.take()
                } else {
                    release_loopback_tls_reservations(
                        &mut state.lower_to_higher_reservations,
                        count,
                    );
                    release_loopback_tls_reservations(
                        &mut state.lower_to_higher_tls_reservations,
                        count,
                    );
                    state.lower_write_waker.take()
                };
                drop(state);
                if let Some(waker) = writer_waker {
                    waker.wake();
                }
                return Ok(count);
            }

            if peer_write_closed || peer_closed {
                return Ok(0);
            }

            let read_interrupted = if self.is_lower_socket {
                std::mem::take(&mut state.lower_read_interrupt)
            } else {
                std::mem::take(&mut state.higher_read_interrupt)
            };
            if read_interrupted {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::WouldBlock,
                    "loopback TLS transport read interrupted",
                ));
            }

            Err(std::io::Error::new(
                std::io::ErrorKind::WouldBlock,
                "loopback TLS transport has no readable ciphertext",
            ))
        }
    }
}

impl Write for crate::state::LoopbackTlsEndpoint {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        let buffered_reservation = self
            .pair
            .resources
            .reserve(ResourceClass::BufferedBytes, buffer.len())
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::WouldBlock, error))?;
        let tls_reservation = self
            .pair
            .resources
            .reserve(ResourceClass::TlsBytes, buffer.len())
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::WouldBlock, error))?;
        let mut state = self
            .pair
            .state
            .lock()
            .map_err(|_| std::io::Error::other("loopback TLS transport lock poisoned"))?;

        let peer_closed = if self.is_lower_socket {
            state.higher_closed
        } else {
            state.lower_closed
        };
        if peer_closed {
            return Err(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "loopback TLS peer is closed",
            ));
        }

        if self.is_lower_socket {
            state.lower_to_higher.extend(buffer.iter().copied());
            state
                .lower_to_higher_reservations
                .push_back(buffered_reservation);
            state
                .lower_to_higher_tls_reservations
                .push_back(tls_reservation);
        } else {
            state.higher_to_lower.extend(buffer.iter().copied());
            state
                .higher_to_lower_reservations
                .push_back(buffered_reservation);
            state
                .higher_to_lower_tls_reservations
                .push_back(tls_reservation);
        }
        let peer_waker = if self.is_lower_socket {
            state.higher_read_waker.take()
        } else {
            state.lower_read_waker.take()
        };
        self.pair.ready.notify_all();
        drop(state);
        if let Some(waker) = peer_waker {
            waker.wake();
        }
        Ok(buffer.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl AsyncRead for crate::state::LoopbackTlsEndpoint {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let endpoint = self.get_mut();
        let mut state = match endpoint.pair.state.lock() {
            Ok(state) => state,
            Err(_) => {
                return Poll::Ready(Err(std::io::Error::other(
                    "loopback TLS transport lock poisoned",
                )));
            }
        };
        let (peer_write_closed, peer_closed) = if endpoint.is_lower_socket {
            (state.higher_write_closed, state.higher_closed)
        } else {
            (state.lower_write_closed, state.lower_closed)
        };
        let incoming = if endpoint.is_lower_socket {
            &mut state.higher_to_lower
        } else {
            &mut state.lower_to_higher
        };
        if !incoming.is_empty() && buffer.remaining() > 0 {
            let count = incoming.len().min(buffer.remaining());
            let destination = buffer.initialize_unfilled_to(count);
            for slot in &mut destination[..count] {
                *slot = incoming.pop_front().expect("loopback TLS incoming byte");
            }
            buffer.advance(count);
            let writer_waker = if endpoint.is_lower_socket {
                release_loopback_tls_reservations(&mut state.higher_to_lower_reservations, count);
                release_loopback_tls_reservations(
                    &mut state.higher_to_lower_tls_reservations,
                    count,
                );
                state.higher_write_waker.take()
            } else {
                release_loopback_tls_reservations(&mut state.lower_to_higher_reservations, count);
                release_loopback_tls_reservations(
                    &mut state.lower_to_higher_tls_reservations,
                    count,
                );
                state.lower_write_waker.take()
            };
            drop(state);
            if let Some(waker) = writer_waker {
                waker.wake();
            }
            return Poll::Ready(Ok(()));
        }
        if peer_write_closed || peer_closed {
            return Poll::Ready(Ok(()));
        }
        if endpoint.is_lower_socket {
            state.lower_read_interrupt = false;
            state.lower_read_waker = Some(cx.waker().clone());
        } else {
            state.higher_read_interrupt = false;
            state.higher_read_waker = Some(cx.waker().clone());
        }
        Poll::Pending
    }
}

impl AsyncWrite for crate::state::LoopbackTlsEndpoint {
    fn poll_write(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        buffer: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        let endpoint = self.get_mut();
        let buffered_reservation = match endpoint
            .pair
            .resources
            .reserve(ResourceClass::BufferedBytes, buffer.len())
        {
            Ok(reservation) => reservation,
            Err(error) => {
                return Poll::Ready(Err(std::io::Error::new(
                    std::io::ErrorKind::WouldBlock,
                    error,
                )));
            }
        };
        let tls_reservation = match endpoint
            .pair
            .resources
            .reserve(ResourceClass::TlsBytes, buffer.len())
        {
            Ok(reservation) => reservation,
            Err(error) => {
                return Poll::Ready(Err(std::io::Error::new(
                    std::io::ErrorKind::WouldBlock,
                    error,
                )));
            }
        };
        let mut state = match endpoint.pair.state.lock() {
            Ok(state) => state,
            Err(_) => {
                return Poll::Ready(Err(std::io::Error::other(
                    "loopback TLS transport lock poisoned",
                )));
            }
        };
        let peer_closed = if endpoint.is_lower_socket {
            state.higher_closed
        } else {
            state.lower_closed
        };
        if peer_closed {
            return Poll::Ready(Err(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "loopback TLS peer is closed",
            )));
        }
        if endpoint.is_lower_socket {
            state.lower_to_higher.extend(buffer.iter().copied());
            state
                .lower_to_higher_reservations
                .push_back(buffered_reservation);
            state
                .lower_to_higher_tls_reservations
                .push_back(tls_reservation);
        } else {
            state.higher_to_lower.extend(buffer.iter().copied());
            state
                .higher_to_lower_reservations
                .push_back(buffered_reservation);
            state
                .higher_to_lower_tls_reservations
                .push_back(tls_reservation);
        }
        let peer_waker = if endpoint.is_lower_socket {
            state.higher_read_waker.take()
        } else {
            state.lower_read_waker.take()
        };
        endpoint.pair.ready.notify_all();
        drop(state);
        if let Some(waker) = peer_waker {
            waker.wake();
        }
        Poll::Ready(Ok(buffer.len()))
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        let endpoint = self.get_mut();
        match endpoint.shutdown_write() {
            Ok(()) => Poll::Ready(Ok(())),
            Err(error) => Poll::Ready(Err(std::io::Error::other(error.to_string()))),
        }
    }
}

// TCP types moved to crate::state

pub(in crate::execution) fn tls_provider() -> Arc<rustls::crypto::CryptoProvider> {
    Arc::new(aws_lc_rs::default_provider())
}

pub(in crate::execution) fn tls_local_certificates(
    options: &TlsBridgeOptions,
) -> Result<Vec<Vec<u8>>, SidecarError> {
    let Some(certificates) = options.cert.as_ref() else {
        return Ok(Vec::new());
    };
    tls_material_entries(certificates)
}

fn tls_material_entries(material: &TlsMaterial) -> Result<Vec<Vec<u8>>, SidecarError> {
    match material {
        TlsMaterial::Single(entry) => tls_data_value(entry).map(|value| vec![value]),
        TlsMaterial::Many(entries) => entries.iter().map(tls_data_value).collect(),
    }
}

fn tls_data_value(value: &TlsDataValue) -> Result<Vec<u8>, SidecarError> {
    match value {
        TlsDataValue::Buffer { data } => base64::engine::general_purpose::STANDARD
            .decode(data)
            .map_err(|error| {
                SidecarError::InvalidState(format!("TLS material contains invalid base64: {error}"))
            }),
        TlsDataValue::String { data } => Ok(data.as_bytes().to_vec()),
    }
}

fn tls_certificates_from_material(
    material: &TlsMaterial,
) -> Result<Vec<CertificateDer<'static>>, SidecarError> {
    let mut certificates = Vec::new();
    for entry in tls_material_entries(material)? {
        let mut reader = std::io::BufReader::new(Cursor::new(entry.clone()));
        let parsed = rustls_pemfile::certs(&mut reader)
            .collect::<Result<Vec<_>, _>>()
            .map_err(sidecar_net_error)?;
        if parsed.is_empty() {
            certificates.push(CertificateDer::from(entry));
        } else {
            certificates.extend(parsed);
        }
    }
    if certificates.is_empty() {
        return Err(SidecarError::InvalidState(String::from(
            "TLS certificate material did not contain any certificates",
        )));
    }
    Ok(certificates)
}

fn tls_private_key_from_material(
    material: &TlsMaterial,
) -> Result<PrivateKeyDer<'static>, SidecarError> {
    for entry in tls_material_entries(material)? {
        let mut reader = std::io::BufReader::new(Cursor::new(entry));
        if let Some(key) = rustls_pemfile::private_key(&mut reader).map_err(sidecar_net_error)? {
            return Ok(key);
        }
    }
    Err(SidecarError::InvalidState(String::from(
        "TLS private key material did not contain a supported key",
    )))
}

pub(in crate::execution) fn vm_default_ca_bundle_for_tls_options(
    kernel: &mut SidecarKernel,
    requester_pid: u32,
    options: &TlsBridgeOptions,
) -> Result<Vec<u8>, SidecarError> {
    if options.is_server || options.reject_unauthorized == Some(false) || options.ca.is_some() {
        return Ok(Vec::new());
    }
    read_vm_default_ca_bundle(kernel, requester_pid)
}

pub(in crate::execution) fn read_vm_default_ca_bundle(
    kernel: &mut SidecarKernel,
    requester_pid: u32,
) -> Result<Vec<u8>, SidecarError> {
    kernel
        .read_file_for_process(
            EXECUTION_DRIVER_NAME,
            requester_pid,
            CA_CERTIFICATES_GUEST_PATH,
        )
        .map_err(kernel_error)
}

#[cfg(test)]
mod ca_bundle_tests {
    use super::*;
    use agentos_kernel::command_registry::CommandDriver;
    use agentos_kernel::kernel::{KernelVmConfig, SpawnOptions};
    use agentos_kernel::mount_table::MountTable;
    use agentos_kernel::permissions::Permissions;
    use agentos_kernel::resource_accounting::ResourceLimits;
    use agentos_kernel::vfs::MemoryFileSystem;

    fn ca_test_kernel() -> (SidecarKernel, u32) {
        let mut config = KernelVmConfig::new("vm-ca-bundle-bounds");
        config.permissions = Permissions::allow_all();
        config.resources = ResourceLimits {
            max_pread_bytes: Some(4),
            ..ResourceLimits::default()
        };
        let mut kernel = SidecarKernel::new(MountTable::new(MemoryFileSystem::new()), config);
        kernel
            .register_driver(CommandDriver::new(EXECUTION_DRIVER_NAME, [WASM_COMMAND]))
            .expect("register CA test driver");
        let process = kernel
            .spawn_process(
                WASM_COMMAND,
                Vec::new(),
                SpawnOptions {
                    requester_driver: Some(String::from(EXECUTION_DRIVER_NAME)),
                    ..SpawnOptions::default()
                },
            )
            .expect("spawn CA test process");
        kernel
            .mkdir("/etc/ssl/certs", true)
            .expect("create CA directory");
        (kernel, process.pid())
    }

    #[test]
    fn live_ca_bundle_read_is_process_authorized_and_bounded() {
        let (mut kernel, pid) = ca_test_kernel();
        kernel
            .write_file(CA_CERTIFICATES_GUEST_PATH, b"four".to_vec())
            .expect("write exact CA bundle");
        assert_eq!(
            read_vm_default_ca_bundle(&mut kernel, pid).expect("read exact CA bundle"),
            b"four"
        );

        kernel
            .write_file(CA_CERTIFICATES_GUEST_PATH, b"five!".to_vec())
            .expect("grow CA bundle");
        let oversized = read_vm_default_ca_bundle(&mut kernel, pid)
            .expect_err("oversized CA bundle must fail before allocation");
        assert_eq!(oversized.code(), Some("EINVAL"));
        assert!(oversized
            .to_string()
            .contains("limits.resources.maxPreadBytes"));

        kernel
            .write_file(CA_CERTIFICATES_GUEST_PATH, b"four".to_vec())
            .expect("restore CA bundle");
        kernel
            .chmod(CA_CERTIFICATES_GUEST_PATH, 0)
            .expect("deny CA bundle");
        assert_eq!(
            read_vm_default_ca_bundle(&mut kernel, pid)
                .expect_err("CA read must enforce process DAC")
                .code(),
            Some("EACCES")
        );

        kernel
            .remove_file(CA_CERTIFICATES_GUEST_PATH)
            .expect("remove CA bundle");
        kernel
            .symlink("/etc/ssl/certs/ca-loop", CA_CERTIFICATES_GUEST_PATH)
            .expect("first CA loop link");
        kernel
            .symlink(CA_CERTIFICATES_GUEST_PATH, "/etc/ssl/certs/ca-loop")
            .expect("second CA loop link");
        assert_eq!(
            read_vm_default_ca_bundle(&mut kernel, pid)
                .expect_err("CA symlink loop must stay typed")
                .code(),
            Some("ELOOP")
        );
    }
}

fn tls_root_store(
    options: &TlsBridgeOptions,
    default_ca_bundle: &[u8],
) -> Result<RootCertStore, SidecarError> {
    let mut roots = RootCertStore::empty();
    if let Some(ca) = options.ca.as_ref() {
        for certificate in tls_certificates_from_material(ca)? {
            roots.add(certificate).map_err(|error| {
                SidecarError::InvalidState(format!("failed to add TLS CA certificate: {error}"))
            })?;
        }
        return Ok(roots);
    }

    let mut reader = std::io::BufReader::new(Cursor::new(default_ca_bundle));
    let certificates = rustls_pemfile::certs(&mut reader)
        .collect::<Result<Vec<_>, _>>()
        .map_err(sidecar_net_error)?;
    if certificates.is_empty() {
        return Err(SidecarError::InvalidState(format!(
            "VM TLS trust store {CA_CERTIFICATES_GUEST_PATH} did not contain any certificates"
        )));
    }
    for certificate in certificates {
        roots.add(certificate).map_err(|error| {
            SidecarError::InvalidState(format!(
                "failed to add VM TLS certificate from {CA_CERTIFICATES_GUEST_PATH} to root store: {error}"
            ))
        })?;
    }
    Ok(roots)
}

pub(in crate::execution) fn build_client_tls_config(
    options: &TlsBridgeOptions,
    default_ca_bundle: &[u8],
) -> Result<ClientConfig, SidecarError> {
    let provider = tls_provider();
    let builder = ClientConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()
        .map_err(|error| {
            SidecarError::InvalidState(format!("invalid TLS protocol config: {error}"))
        })?;

    let mut config = if options.reject_unauthorized == Some(false) {
        let verifier = Arc::new(InsecureTlsVerifier {
            supported_schemes: provider
                .signature_verification_algorithms
                .supported_schemes(),
        });
        builder
            .dangerous()
            .with_custom_certificate_verifier(verifier)
            .with_no_client_auth()
    } else {
        builder
            .with_root_certificates(tls_root_store(options, default_ca_bundle)?)
            .with_no_client_auth()
    };

    if let Some(protocols) = options.alpn_protocols.as_ref() {
        config.alpn_protocols = protocols
            .iter()
            .map(|protocol| protocol.as_bytes().to_vec())
            .collect();
    }
    config.enable_sni = options.servername.as_deref() != Some("");
    Ok(config)
}

pub(in crate::execution) fn build_server_tls_config(
    options: &TlsBridgeOptions,
) -> Result<ServerConfig, SidecarError> {
    let certificates = tls_certificates_from_material(options.cert.as_ref().ok_or_else(|| {
        SidecarError::InvalidState(String::from("TLS server upgrade requires a certificate"))
    })?)?;
    let key = tls_private_key_from_material(options.key.as_ref().ok_or_else(|| {
        SidecarError::InvalidState(String::from("TLS server upgrade requires a private key"))
    })?)?;

    let mut config = ServerConfig::builder_with_provider(tls_provider())
        .with_safe_default_protocol_versions()
        .map_err(|error| {
            SidecarError::InvalidState(format!("invalid TLS protocol config: {error}"))
        })?
        .with_no_client_auth()
        .with_single_cert(certificates, key)
        .map_err(|error| {
            SidecarError::InvalidState(format!("invalid TLS server config: {error}"))
        })?;

    if let Some(protocols) = options.alpn_protocols.as_ref() {
        config.alpn_protocols = protocols
            .iter()
            .map(|protocol| protocol.as_bytes().to_vec())
            .collect();
    }
    Ok(config)
}

fn tls_protocol_name(version: rustls::ProtocolVersion) -> String {
    match version {
        rustls::ProtocolVersion::TLSv1_2 => String::from("TLSv1.2"),
        rustls::ProtocolVersion::TLSv1_3 => String::from("TLSv1.3"),
        other => other
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| format!("{other:?}")),
    }
}

fn tls_cipher_bridge_value(suite: rustls::SupportedCipherSuite) -> Value {
    tls_bridge_object(vec![
        (
            "name",
            suite
                .suite()
                .as_str()
                .map(|value| Value::String(value.to_owned()))
                .unwrap_or(Value::Null),
        ),
        (
            "standardName",
            suite
                .suite()
                .as_str()
                .map(|value| Value::String(value.to_owned()))
                .unwrap_or(Value::Null),
        ),
        (
            "version",
            Value::String(if suite.tls13().is_some() {
                String::from("TLSv1.3")
            } else {
                String::from("TLSv1.2")
            }),
        ),
    ])
}

pub(in crate::execution) fn tls_certificate_bridge_value(
    certificate: &[u8],
    detailed: bool,
) -> Value {
    let mut fields = vec![("raw", tls_bridge_buffer_value(certificate))];
    if detailed {
        fields.push(("issuerCertificate", tls_bridge_undefined_value()));
    }
    tls_bridge_object(fields)
}

fn tls_bridge_buffer_value(bytes: &[u8]) -> Value {
    json!({
        "type": "buffer",
        "data": base64::engine::general_purpose::STANDARD.encode(bytes),
    })
}

fn tls_bridge_object(entries: Vec<(&str, Value)>) -> Value {
    let value = entries
        .into_iter()
        .map(|(key, value)| (key.to_owned(), value))
        .collect::<serde_json::Map<String, Value>>();
    json!({
        "type": "object",
        "id": 1,
        "value": value,
    })
}

pub(in crate::execution) fn tls_bridge_undefined_value() -> Value {
    json!({
        "type": "undefined",
    })
}

pub(in crate::execution) enum NativeTlsRole {
    Client {
        config: Arc<ClientConfig>,
        server_name: ServerName<'static>,
    },
    Server {
        config: Arc<ServerConfig>,
    },
}

const SOCKET_READ_BUFFER_BYTES: usize = 64 * 1024;

/// Reserve the complete backing allocation before creating a reusable socket
/// read buffer. When a read result is copied into an event, the event acquires
/// its own exact reservation first, so the source and destination allocations
/// are both charged during the copy.
pub(in crate::execution) fn reserve_socket_read_buffer(
    resources: &ResourceLedger,
    byte_quantum: usize,
) -> Result<(Vec<u8>, Reservation), SidecarError> {
    let configured_limit = resources
        .usage(ResourceClass::BufferedBytes)
        .limit
        .unwrap_or(SOCKET_READ_BUFFER_BYTES);
    let capacity = configured_limit
        .min(SOCKET_READ_BUFFER_BYTES)
        .min(byte_quantum)
        .max(1);
    let reservation = resources
        .reserve(ResourceClass::BufferedBytes, capacity)
        .map_err(SidecarError::from)?;
    let buffer = vec![0_u8; capacity];
    Ok((buffer, reservation))
}

fn reserve_tls_read_buffer(
    resources: &ResourceLedger,
    limits: ReactorIoLimits,
) -> Result<(Vec<u8>, Reservation, Reservation), SidecarError> {
    // The reusable decrypt destination and the guest-bound event copy coexist.
    // Reserve no more than half either budget so one completed read can always
    // transfer without deadlocking behind its own source buffer.
    let aggregate_limit = resources
        .usage(ResourceClass::BufferedBytes)
        .limit
        .unwrap_or(SOCKET_READ_BUFFER_BYTES * 2);
    let tls_limit = resources
        .usage(ResourceClass::TlsBytes)
        .limit
        .unwrap_or(SOCKET_READ_BUFFER_BYTES * 2);
    let capacity = SOCKET_READ_BUFFER_BYTES
        .min(limits.byte_quantum)
        .min((aggregate_limit / 2).max(1))
        .min((tls_limit / 2).max(1))
        .max(1);
    let buffered_reservation = resources
        .reserve(ResourceClass::BufferedBytes, capacity)
        .map_err(SidecarError::from)?;
    let tls_reservation = resources
        .reserve(ResourceClass::TlsBytes, capacity)
        .map_err(SidecarError::from)?;
    let buffer = vec![0_u8; capacity];
    Ok((buffer, buffered_reservation, tls_reservation))
}

pub(crate) fn reserve_tls_write_payload(
    resources: &ResourceLedger,
    contents: &[u8],
) -> Result<TlsWritePayload, SidecarError> {
    let command_reservation = resources
        .reserve(ResourceClass::HandleCommands, 1)
        .map_err(SidecarError::from)?;
    let command_bytes_reservation = resources
        .reserve(ResourceClass::HandleCommandBytes, contents.len())
        .map_err(SidecarError::from)?;
    let buffered_reservation = resources
        .reserve(ResourceClass::BufferedBytes, contents.len())
        .map_err(SidecarError::from)?;
    let tls_reservation = resources
        .reserve(ResourceClass::TlsBytes, contents.len())
        .map_err(SidecarError::from)?;
    Ok(TlsWritePayload {
        bytes: contents.to_vec(),
        _command_reservation: SharedReservation::new(command_reservation),
        _command_bytes_reservation: SharedReservation::new(command_bytes_reservation),
        _buffered_reservation: SharedReservation::new(buffered_reservation),
        _tls_reservation: SharedReservation::new(tls_reservation),
    })
}

pub(in crate::execution) fn reserve_tls_command(
    resources: &ResourceLedger,
) -> Result<SharedReservation, SidecarError> {
    resources
        .reserve(ResourceClass::HandleCommands, 1)
        .map(SharedReservation::new)
        .map_err(SidecarError::from)
}

pub(in crate::execution) fn native_tls_role(
    options: &TlsBridgeOptions,
    default_ca_bundle: &[u8],
) -> Result<NativeTlsRole, SidecarError> {
    if options.is_server {
        return Ok(NativeTlsRole::Server {
            config: Arc::new(build_server_tls_config(options)?),
        });
    }

    let server_name = options
        .servername
        .as_deref()
        .filter(|value| !value.is_empty())
        .or(options.host.as_deref())
        .map(str::to_owned)
        .unwrap_or_else(|| String::from("localhost"));
    let server_name = ServerName::try_from(server_name)
        .map_err(|_| SidecarError::InvalidState(String::from("invalid TLS servername")))?;
    Ok(NativeTlsRole::Client {
        config: Arc::new(build_client_tls_config(options, default_ca_bundle)?),
        server_name,
    })
}

fn update_native_tls_negotiated_state(
    state: &Arc<Mutex<Option<ActiveTlsState>>>,
    peer_certificates: Option<&[CertificateDer<'static>]>,
    protocol: Option<rustls::ProtocolVersion>,
    cipher: Option<rustls::SupportedCipherSuite>,
) {
    let Ok(mut state) = state.lock() else {
        eprintln!("ERR_AGENTOS_TLS_STATE_POISONED: could not record negotiated TLS state");
        return;
    };
    let Some(state) = state.as_mut() else {
        eprintln!("ERR_AGENTOS_TLS_STATE_MISSING: negotiated TLS state has no socket owner");
        return;
    };
    state.peer_certificates = peer_certificates
        .unwrap_or_default()
        .iter()
        .map(|certificate| certificate.as_ref().to_vec())
        .collect();
    state.protocol = protocol.map(tls_protocol_name);
    state.cipher = cipher.map(tls_cipher_bridge_value);
}

fn tls_transport_is_already_closed(error: &std::io::Error) -> bool {
    matches!(
        error.kind(),
        std::io::ErrorKind::BrokenPipe
            | std::io::ErrorKind::ConnectionReset
            | std::io::ErrorKind::NotConnected
            | std::io::ErrorKind::UnexpectedEof
    )
}

#[allow(
    clippy::too_many_arguments,
    reason = "the transport task receives explicit shared lifecycle flags owned by its socket"
)]
async fn run_native_tls_transport<S>(
    mut stream: S,
    mut commands: TokioReceiver<NativeTlsCommand>,
    sender: AsyncCompletionSender<TcpSocketEvent>,
    event_pusher: Arc<SocketReadinessSubscribers>,
    application_read_interest: Arc<AtomicBool>,
    application_read_notify: Arc<tokio::sync::Notify>,
    saw_local_shutdown: Arc<AtomicBool>,
    saw_remote_end: Arc<AtomicBool>,
    close_notified: Arc<AtomicBool>,
    resources: Arc<ResourceLedger>,
    mut buffer: Vec<u8>,
    _read_buffer_reservation: Reservation,
    _tls_read_buffer_reservation: Reservation,
    limits: ReactorIoLimits,
    runtime_context: agentos_runtime::RuntimeContext,
    fairness_identity: (u64, u64),
) where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut operations_this_turn = 0;
    loop {
        let read_interested = application_read_interest.load(Ordering::Acquire);
        tokio::select! {
            biased;
            command = commands.recv() => {
                let Some(command) = command else {
                    if let Err(error) = AsyncWriteExt::shutdown(&mut stream).await {
                        if !tls_transport_is_already_closed(&error) {
                            eprintln!(
                                "ERR_AGENTOS_TLS_SHUTDOWN: transport command channel closed during shutdown: {error}"
                            );
                        }
                    }
                    break;
                };
                match command {
                    NativeTlsCommand::Write { payload, completion } => {
                        let payload_len = payload.bytes.len();
                        let result = crate::execution::operation_deadline_timeout(
                            "TLS socket write",
                            limits.operation_deadline,
                            async {
                            let mut offset = 0;
                            while offset < payload.bytes.len() {
                                let (capability_id, vm_generation) = fairness_identity;
                                let turn = runtime_context.fairness().acquire(
                                    vm_generation,
                                    capability_id,
                                    FairBudget::new(
                                        limits.operation_quantum.max(1),
                                        limits.byte_quantum.max(1),
                                    ),
                                ).await.map_err(std::io::Error::other)?;
                                let chunk_len = turn
                                    .allowance()
                                    .bytes
                                    .min(limits.byte_quantum.max(1))
                                    .min(payload.bytes.len() - offset)
                                    .max(1);
                                turn.complete(FairBudget::new(1, chunk_len), false)
                                    .map_err(std::io::Error::other)?;
                                let chunk = &payload.bytes[offset..offset + chunk_len];
                                AsyncWriteExt::write_all(&mut stream, chunk).await?;
                                offset += chunk_len;
                            }
                            AsyncWriteExt::flush(&mut stream).await?;
                            Ok(payload_len)
                        })
                        .await
                        .map_err(|_| format!(
                            "ERR_AGENTOS_OPERATION_DEADLINE: TLS write exceeded {}ms; raise limits.reactor.operationDeadlineMs",
                            limits.operation_deadline.as_millis()
                        ))
                        .and_then(|result: Result<usize, std::io::Error>| {
                            result.map_err(|error| error.to_string())
                        })
                        .map(Value::from)
                        .map_err(|message| {
                            deferred_rpc_error("ERR_AGENTOS_TLS_WRITE", message)
                        });
                        if let Some(completion) = completion {
                            if completion.send(result).is_err() {
                                eprintln!("ERR_AGENTOS_TLS_COMPLETION_DROPPED: TLS write caller stopped waiting");
                            }
                        } else if let Err(error) = result {
                            send_async_socket_error_and_close(
                                &sender,
                                &event_pusher,
                                &close_notified,
                                Some(error.code),
                                error.message,
                            )
                            .await;
                            break;
                        }
                    }
                    NativeTlsCommand::Shutdown {
                        _command_reservation: _,
                        completion,
                    } => {
                        let result = crate::execution::operation_deadline_timeout(
                            "TLS socket shutdown",
                            limits.operation_deadline,
                            async {
                                let (capability_id, vm_generation) = fairness_identity;
                                let turn = runtime_context
                                    .fairness()
                                    .acquire(
                                        vm_generation,
                                        capability_id,
                                        FairBudget::new(
                                            limits.operation_quantum.max(1),
                                            limits.byte_quantum.max(1),
                                        ),
                                    )
                                    .await
                                    .map_err(std::io::Error::other)?;
                                turn.complete(FairBudget::new(1, 0), false)
                                    .map_err(std::io::Error::other)?;
                                AsyncWriteExt::shutdown(&mut stream).await
                            },
                        )
                        .await
                        .map_err(|_| format!(
                            "ERR_AGENTOS_OPERATION_DEADLINE: TLS shutdown exceeded {}ms; raise limits.reactor.operationDeadlineMs",
                            limits.operation_deadline.as_millis()
                        ))
                        .and_then(|result| result.map_err(|error| error.to_string()))
                        .map(|()| Value::Null)
                        .map_err(|message| {
                            deferred_rpc_error("ERR_AGENTOS_TLS_SHUTDOWN", message)
                        });
                        saw_local_shutdown.store(true, Ordering::SeqCst);
                        if completion.send(result).is_err() {
                            eprintln!("ERR_AGENTOS_TLS_COMPLETION_DROPPED: TLS shutdown caller stopped waiting");
                        }
                    }
                    NativeTlsCommand::Close {
                        _command_reservation: _,
                    } => {
                        // Handle retirement closes fairness admission before
                        // this queued cleanup command runs. Teardown must not
                        // reacquire a retired capability; the bounded command
                        // reservation and operation deadline already govern the
                        // final transport shutdown.
                        let result = crate::execution::operation_deadline_timeout(
                            "TLS socket close",
                            limits.operation_deadline,
                            AsyncWriteExt::shutdown(&mut stream),
                        )
                        .await;
                        match result {
                            Ok(Ok(())) => {}
                            Ok(Err(error)) if tls_transport_is_already_closed(&error) => {}
                            Ok(Err(error)) => eprintln!("ERR_AGENTOS_TLS_CLOSE: {error}"),
                            Err(_) => eprintln!(
                                "ERR_AGENTOS_OPERATION_DEADLINE: TLS close exceeded {}ms; raise limits.reactor.operationDeadlineMs",
                                limits.operation_deadline.as_millis()
                            ),
                        }
                        break;
                    }
                }
            }
            _ = application_read_notify.notified(), if !read_interested => {}
            read_result = AsyncReadExt::read(&mut stream, &mut buffer), if read_interested => {
                match read_result {
                    Ok(0) => {
                        saw_remote_end.store(true, Ordering::SeqCst);
                        if sender.send(TcpSocketEvent::End).await.is_ok() {
                            push_socket_event(&event_pusher, "end");
                        }
                        if saw_local_shutdown.load(Ordering::SeqCst)
                            && !close_notified.swap(true, Ordering::SeqCst)
                            && sender
                                .send(TcpSocketEvent::Close { had_error: false })
                                .await
                                .is_ok()
                        {
                            push_socket_event(&event_pusher, "close");
                        }
                        break;
                    }
                    Ok(bytes_read) => {
                        let Some((reservation, tls_reservation)) = reserve_tls_event_bytes_or_close(
                            &resources,
                            bytes_read,
                            &sender,
                            &event_pusher,
                            &close_notified,
                        )
                        .await
                        else {
                            break;
                        };
                        let (capability_id, vm_generation) = fairness_identity;
                        let turn = match runtime_context
                            .fairness()
                            .acquire(
                                vm_generation,
                                capability_id,
                                FairBudget::new(
                                    limits.operation_quantum.max(1),
                                    limits.byte_quantum.max(1),
                                ),
                            )
                            .await
                        {
                            Ok(turn) => turn,
                            Err(error) => {
                                send_async_socket_error_and_close(
                                    &sender,
                                    &event_pusher,
                                    &close_notified,
                                    Some(String::from("ERR_AGENTOS_FAIRNESS")),
                                    error.to_string(),
                                )
                                .await;
                                break;
                            }
                        };
                        if bytes_read > turn.allowance().bytes {
                            let allowance = turn.allowance().bytes;
                            drop(turn);
                            send_async_socket_error_and_close(
                                &sender,
                                &event_pusher,
                                &close_notified,
                                Some(String::from("ERR_AGENTOS_FAIRNESS_BYTE_BUDGET")),
                                format!(
                                    "TLS read produced {bytes_read} bytes beyond fair allowance {}; lower limits.reactor.byteQuantum or raise runtime.fairness.capabilityQuantumBytes",
                                    allowance
                                ),
                            )
                            .await;
                            break;
                        }
                        let event = TcpSocketEvent::Data {
                            bytes: buffer[..bytes_read].to_vec(),
                            reservation: SharedReservation::new(reservation),
                            source_reservations: vec![SharedReservation::new(tls_reservation)],
                        };
                        if let Err(error) = turn.complete(FairBudget::new(1, bytes_read), false) {
                            send_async_socket_error_and_close(
                                &sender,
                                &event_pusher,
                                &close_notified,
                                Some(String::from("ERR_AGENTOS_FAIRNESS")),
                                error.to_string(),
                            )
                            .await;
                            break;
                        }
                        if sender
                            .send(event)
                            .await
                            .is_err()
                        {
                            break;
                        }
                        push_socket_event(&event_pusher, "data");
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => {
                        saw_remote_end.store(true, Ordering::SeqCst);
                        if sender.send(TcpSocketEvent::End).await.is_ok() {
                            push_socket_event(&event_pusher, "end");
                        }
                        if saw_local_shutdown.load(Ordering::SeqCst)
                            && !close_notified.swap(true, Ordering::SeqCst)
                            && sender
                                .send(TcpSocketEvent::Close { had_error: false })
                                .await
                                .is_ok()
                        {
                            push_socket_event(&event_pusher, "close");
                        }
                        break;
                    }
                    Err(error) => {
                        send_async_socket_error_and_close(
                            &sender,
                            &event_pusher,
                            &close_notified,
                            io_error_code(&error),
                            error.to_string(),
                        )
                        .await;
                        break;
                    }
                }
            }
        }
        operations_this_turn += 1;
        if operations_this_turn >= limits.operation_quantum.max(1) {
            tokio::task::yield_now().await;
            operations_this_turn = 0;
        }
    }
}

type TlsTransportRegistration = Result<
    (
        TokioSender<NativeTlsCommand>,
        tokio::sync::oneshot::Receiver<Result<Value, crate::state::DeferredRpcError>>,
    ),
    SidecarError,
>;

#[allow(clippy::too_many_arguments)]
pub(in crate::execution) fn spawn_native_tls_transport(
    runtime: agentos_runtime::RuntimeContext,
    stream: TcpStream,
    role: NativeTlsRole,
    tls_state: Arc<Mutex<Option<ActiveTlsState>>>,
    sender: AsyncCompletionSender<TcpSocketEvent>,
    event_pusher: Arc<SocketReadinessSubscribers>,
    application_read_interest: Arc<AtomicBool>,
    application_read_notify: Arc<tokio::sync::Notify>,
    plain_reader_running: Arc<AtomicBool>,
    plain_reader_stopped: Arc<tokio::sync::Notify>,
    saw_local_shutdown: Arc<AtomicBool>,
    saw_remote_end: Arc<AtomicBool>,
    close_notified: Arc<AtomicBool>,
    resources: Arc<ResourceLedger>,
    limits: ReactorIoLimits,
    fairness_identity: (u64, u64),
) -> TlsTransportRegistration {
    let (command_tx, command_rx) = tokio_channel(limits.max_handle_commands);
    let (handshake_tx, handshake_rx) = tokio::sync::oneshot::channel();
    let (read_buffer, read_buffer_reservation, tls_read_buffer_reservation) =
        reserve_tls_read_buffer(&resources, limits)?;
    let transport_runtime = runtime.clone();
    runtime
        .spawn(agentos_runtime::TaskClass::Tls, async move {
            while plain_reader_running.load(Ordering::Acquire) {
                let stopped = plain_reader_stopped.notified();
                if !plain_reader_running.load(Ordering::Acquire) {
                    break;
                }
                stopped.await;
            }

            if let Err(error) = stream.set_nonblocking(true) {
                let message = error.to_string();
                send_oneshot_or_log(
                    handshake_tx,
                    Err(deferred_rpc_error(
                        "ERR_AGENTOS_TLS_HANDSHAKE",
                        message.clone(),
                    )),
                    "native TLS socket setup failure",
                );
                send_async_socket_error_and_close(
                    &sender,
                    &event_pusher,
                    &close_notified,
                    io_error_code(&error),
                    message,
                )
                .await;
                return;
            }
            let stream = match tokio::net::TcpStream::from_std(stream) {
                Ok(stream) => stream,
                Err(error) => {
                    let message = error.to_string();
                    send_oneshot_or_log(
                        handshake_tx,
                        Err(deferred_rpc_error(
                            "ERR_AGENTOS_TLS_HANDSHAKE",
                            message.clone(),
                        )),
                        "native TLS runtime socket setup failure",
                    );
                    send_async_socket_error_and_close(
                        &sender,
                        &event_pusher,
                        &close_notified,
                        io_error_code(&error),
                        message,
                    )
                    .await;
                    return;
                }
            };

            match role {
                NativeTlsRole::Client {
                    config,
                    server_name,
                } => {
                    let handshake = crate::execution::operation_deadline_timeout(
                        "TLS client handshake",
                        limits.operation_deadline,
                        TlsConnector::from(config).connect(server_name, stream),
                    )
                    .await;
                    let tls_stream = match handshake {
                        Ok(Ok(stream)) => stream,
                        Ok(Err(error)) => {
                            let message = error.to_string();
                            send_oneshot_or_log(
                                handshake_tx,
                                Err(deferred_rpc_error(
                                    "ERR_AGENTOS_TLS_HANDSHAKE",
                                    message.clone(),
                                )),
                                "native TLS client handshake failure",
                            );
                            send_async_socket_error_and_close(
                                &sender,
                                &event_pusher,
                                &close_notified,
                                io_error_code(&error),
                                message,
                            )
                            .await;
                            return;
                        }
                        Err(_) => {
                            let message = format!(
                                "TLS handshake timed out after {}ms",
                                limits.operation_deadline.as_millis()
                            );
                            send_oneshot_or_log(
                                handshake_tx,
                                Err(deferred_rpc_error(
                                    "ERR_AGENTOS_TLS_HANDSHAKE",
                                    message.clone(),
                                )),
                                "native TLS client handshake timeout",
                            );
                            send_async_socket_error_and_close(
                                &sender,
                                &event_pusher,
                                &close_notified,
                                Some(String::from("ETIMEDOUT")),
                                message,
                            )
                            .await;
                            return;
                        }
                    };
                    let (_, connection) = tls_stream.get_ref();
                    update_native_tls_negotiated_state(
                        &tls_state,
                        connection.peer_certificates(),
                        connection.protocol_version(),
                        connection.negotiated_cipher_suite(),
                    );
                    if handshake_tx.send(Ok(Value::Null)).is_err() {
                        eprintln!(
                        "ERR_AGENTOS_TLS_COMPLETION_DROPPED: TLS handshake caller stopped waiting"
                    );
                        return;
                    }
                    run_native_tls_transport(
                        tls_stream,
                        command_rx,
                        sender,
                        event_pusher,
                        application_read_interest,
                        application_read_notify,
                        saw_local_shutdown,
                        saw_remote_end,
                        close_notified,
                        resources,
                        read_buffer,
                        read_buffer_reservation,
                        tls_read_buffer_reservation,
                        limits,
                        transport_runtime,
                        fairness_identity,
                    )
                    .await;
                }
                NativeTlsRole::Server { config } => {
                    let handshake = crate::execution::operation_deadline_timeout(
                        "TLS server handshake",
                        limits.operation_deadline,
                        TlsAcceptor::from(config).accept(stream),
                    )
                    .await;
                    let tls_stream = match handshake {
                        Ok(Ok(stream)) => stream,
                        Ok(Err(error)) => {
                            let message = error.to_string();
                            send_oneshot_or_log(
                                handshake_tx,
                                Err(deferred_rpc_error(
                                    "ERR_AGENTOS_TLS_HANDSHAKE",
                                    message.clone(),
                                )),
                                "native TLS server handshake failure",
                            );
                            send_async_socket_error_and_close(
                                &sender,
                                &event_pusher,
                                &close_notified,
                                io_error_code(&error),
                                message,
                            )
                            .await;
                            return;
                        }
                        Err(_) => {
                            let message = format!(
                                "TLS handshake timed out after {}ms",
                                limits.operation_deadline.as_millis()
                            );
                            send_oneshot_or_log(
                                handshake_tx,
                                Err(deferred_rpc_error(
                                    "ERR_AGENTOS_TLS_HANDSHAKE",
                                    message.clone(),
                                )),
                                "native TLS server handshake timeout",
                            );
                            send_async_socket_error_and_close(
                                &sender,
                                &event_pusher,
                                &close_notified,
                                Some(String::from("ETIMEDOUT")),
                                message,
                            )
                            .await;
                            return;
                        }
                    };
                    let (_, connection) = tls_stream.get_ref();
                    update_native_tls_negotiated_state(
                        &tls_state,
                        connection.peer_certificates(),
                        connection.protocol_version(),
                        connection.negotiated_cipher_suite(),
                    );
                    if handshake_tx.send(Ok(Value::Null)).is_err() {
                        eprintln!(
                        "ERR_AGENTOS_TLS_COMPLETION_DROPPED: TLS handshake caller stopped waiting"
                    );
                        return;
                    }
                    run_native_tls_transport(
                        tls_stream,
                        command_rx,
                        sender,
                        event_pusher,
                        application_read_interest,
                        application_read_notify,
                        saw_local_shutdown,
                        saw_remote_end,
                        close_notified,
                        resources,
                        read_buffer,
                        read_buffer_reservation,
                        tls_read_buffer_reservation,
                        limits,
                        transport_runtime,
                        fairness_identity,
                    )
                    .await;
                }
            }
        })
        .map_err(|error| SidecarError::Execution(error.to_string()))?;

    Ok((command_tx, handshake_rx))
}

#[allow(clippy::too_many_arguments)]
pub(in crate::execution) fn spawn_loopback_tls_transport(
    runtime: agentos_runtime::RuntimeContext,
    endpoint: crate::state::LoopbackTlsEndpoint,
    role: NativeTlsRole,
    tls_state: Arc<Mutex<Option<ActiveTlsState>>>,
    sender: AsyncCompletionSender<TcpSocketEvent>,
    event_pusher: Arc<SocketReadinessSubscribers>,
    application_read_interest: Arc<AtomicBool>,
    application_read_notify: Arc<tokio::sync::Notify>,
    saw_local_shutdown: Arc<AtomicBool>,
    saw_remote_end: Arc<AtomicBool>,
    close_notified: Arc<AtomicBool>,
    resources: Arc<ResourceLedger>,
    limits: ReactorIoLimits,
    fairness_identity: (u64, u64),
) -> TlsTransportRegistration {
    let (command_tx, command_rx) = tokio_channel(limits.max_handle_commands);
    let (handshake_tx, handshake_rx) = tokio::sync::oneshot::channel();
    let (read_buffer, read_buffer_reservation, tls_read_buffer_reservation) =
        reserve_tls_read_buffer(&resources, limits)?;
    let transport_runtime = runtime.clone();
    runtime
        .spawn(agentos_runtime::TaskClass::Tls, async move {
            match role {
                NativeTlsRole::Client {
                    config,
                    server_name,
                } => {
                    let handshake = crate::execution::operation_deadline_timeout(
                        "TLS client handshake",
                        limits.operation_deadline,
                        TlsConnector::from(config).connect(server_name, endpoint),
                    )
                    .await;
                    let tls_stream = match handshake {
                        Ok(Ok(stream)) => stream,
                        Ok(Err(error)) => {
                            let message = error.to_string();
                            send_oneshot_or_log(
                                handshake_tx,
                                Err(deferred_rpc_error(
                                    "ERR_AGENTOS_TLS_HANDSHAKE",
                                    message.clone(),
                                )),
                                "loopback TLS client handshake failure",
                            );
                            send_async_socket_error_and_close(
                                &sender,
                                &event_pusher,
                                &close_notified,
                                io_error_code(&error),
                                message,
                            )
                            .await;
                            return;
                        }
                        Err(_) => {
                            let message = format!(
                                "loopback TLS handshake timed out after {}ms",
                                limits.operation_deadline.as_millis()
                            );
                            send_oneshot_or_log(
                                handshake_tx,
                                Err(deferred_rpc_error(
                                    "ERR_AGENTOS_TLS_HANDSHAKE",
                                    message.clone(),
                                )),
                                "loopback TLS client handshake timeout",
                            );
                            send_async_socket_error_and_close(
                                &sender,
                                &event_pusher,
                                &close_notified,
                                Some(String::from("ETIMEDOUT")),
                                message,
                            )
                            .await;
                            return;
                        }
                    };
                    let (_, connection) = tls_stream.get_ref();
                    update_native_tls_negotiated_state(
                        &tls_state,
                        connection.peer_certificates(),
                        connection.protocol_version(),
                        connection.negotiated_cipher_suite(),
                    );
                    if handshake_tx.send(Ok(Value::Null)).is_err() {
                        eprintln!(
                            "ERR_AGENTOS_TLS_COMPLETION_DROPPED: loopback TLS handshake caller stopped waiting"
                        );
                        return;
                    }
                    run_native_tls_transport(
                        tls_stream,
                        command_rx,
                        sender,
                        event_pusher,
                        application_read_interest,
                        application_read_notify,
                        saw_local_shutdown,
                        saw_remote_end,
                        close_notified,
                        resources,
                        read_buffer,
                        read_buffer_reservation,
                        tls_read_buffer_reservation,
                        limits,
                        transport_runtime,
                        fairness_identity,
                    )
                    .await;
                }
                NativeTlsRole::Server { config } => {
                    let handshake = crate::execution::operation_deadline_timeout(
                        "TLS server handshake",
                        limits.operation_deadline,
                        TlsAcceptor::from(config).accept(endpoint),
                    )
                    .await;
                    let tls_stream = match handshake {
                        Ok(Ok(stream)) => stream,
                        Ok(Err(error)) => {
                            let message = error.to_string();
                            send_oneshot_or_log(
                                handshake_tx,
                                Err(deferred_rpc_error(
                                    "ERR_AGENTOS_TLS_HANDSHAKE",
                                    message.clone(),
                                )),
                                "loopback TLS server handshake failure",
                            );
                            send_async_socket_error_and_close(
                                &sender,
                                &event_pusher,
                                &close_notified,
                                io_error_code(&error),
                                message,
                            )
                            .await;
                            return;
                        }
                        Err(_) => {
                            let message = format!(
                                "loopback TLS handshake timed out after {}ms",
                                limits.operation_deadline.as_millis()
                            );
                            send_oneshot_or_log(
                                handshake_tx,
                                Err(deferred_rpc_error(
                                    "ERR_AGENTOS_TLS_HANDSHAKE",
                                    message.clone(),
                                )),
                                "loopback TLS server handshake timeout",
                            );
                            send_async_socket_error_and_close(
                                &sender,
                                &event_pusher,
                                &close_notified,
                                Some(String::from("ETIMEDOUT")),
                                message,
                            )
                            .await;
                            return;
                        }
                    };
                    let (_, connection) = tls_stream.get_ref();
                    update_native_tls_negotiated_state(
                        &tls_state,
                        connection.peer_certificates(),
                        connection.protocol_version(),
                        connection.negotiated_cipher_suite(),
                    );
                    if handshake_tx.send(Ok(Value::Null)).is_err() {
                        eprintln!(
                            "ERR_AGENTOS_TLS_COMPLETION_DROPPED: loopback TLS handshake caller stopped waiting"
                        );
                        return;
                    }
                    run_native_tls_transport(
                        tls_stream,
                        command_rx,
                        sender,
                        event_pusher,
                        application_read_interest,
                        application_read_notify,
                        saw_local_shutdown,
                        saw_remote_end,
                        close_notified,
                        resources,
                        read_buffer,
                        read_buffer_reservation,
                        tls_read_buffer_reservation,
                        limits,
                        transport_runtime,
                        fairness_identity,
                    )
                    .await;
                }
            }
        })
        .map_err(|error| SidecarError::Execution(error.to_string()))?;
    Ok((command_tx, handshake_rx))
}

#[cfg(test)]
mod loopback_tls_registry_tests {
    use super::{
        loopback_tls_endpoint, loopback_tls_registry_contains, loopback_tls_transport_key,
        native_tls_role, tls_transport_is_already_closed, NativeTlsRole,
    };
    use crate::state::TlsBridgeOptions;
    use agentos_runtime::accounting::{ResourceClass, ResourceLedger, ResourceLimit};
    use std::sync::Arc;

    fn resources() -> Arc<ResourceLedger> {
        Arc::new(ResourceLedger::root(
            "loopback-tls-test",
            [(
                ResourceClass::BufferedBytes,
                ResourceLimit::new(1024 * 1024, "limits.resources.maxSocketBufferedBytes"),
            )],
        ))
    }

    #[test]
    fn empty_servername_uses_ip_host_without_sni() {
        let options = TlsBridgeOptions {
            host: Some(String::from("127.0.0.1")),
            servername: Some(String::new()),
            reject_unauthorized: Some(false),
            ..TlsBridgeOptions::default()
        };
        let NativeTlsRole::Client {
            config,
            server_name,
        } = native_tls_role(&options, &[]).expect("build IP TLS client role")
        else {
            panic!("expected client TLS role");
        };

        assert_eq!(server_name.to_str(), "127.0.0.1");
        assert!(!config.enable_sni);
    }

    #[test]
    fn tls_close_classifies_only_terminal_transport_states_as_idempotent() {
        for kind in [
            std::io::ErrorKind::BrokenPipe,
            std::io::ErrorKind::ConnectionReset,
            std::io::ErrorKind::NotConnected,
            std::io::ErrorKind::UnexpectedEof,
        ] {
            assert!(tls_transport_is_already_closed(&std::io::Error::from(kind)));
        }
        assert!(!tls_transport_is_already_closed(&std::io::Error::from(
            std::io::ErrorKind::TimedOut,
        )));
    }

    // Each test uses a unique vm_id so the process-global registry stays
    // partitioned across concurrently running tests.

    #[test]
    fn dropping_endpoint_removes_its_registry_entry() {
        let vm_id = "loopback-tls-drop-removes-entry";
        let key = loopback_tls_transport_key(vm_id, 1, 2);

        let endpoint = loopback_tls_endpoint(vm_id, 1, 2, resources()).expect("create endpoint");
        assert!(
            loopback_tls_registry_contains(&key),
            "registry should contain the key while the endpoint is alive"
        );

        drop(endpoint);
        assert!(
            !loopback_tls_registry_contains(&key),
            "registry entry must be pruned in the endpoint's Drop, not left for the lazy retain()"
        );
    }

    #[test]
    fn registry_entry_survives_until_last_peer_endpoint_drops() {
        let vm_id = "loopback-tls-shared-pair";
        let key = loopback_tls_transport_key(vm_id, 3, 4);

        // Both peers of a loopback connection share the same transport pair.
        let resources = resources();
        let lower = loopback_tls_endpoint(vm_id, 3, 4, Arc::clone(&resources))
            .expect("create lower endpoint");
        let higher = loopback_tls_endpoint(vm_id, 4, 3, resources).expect("create higher endpoint");
        assert!(loopback_tls_registry_contains(&key));

        // Dropping one peer must keep the entry, since the other peer still owns
        // the shared pair and a later connection must be able to find it.
        drop(lower);
        assert!(
            loopback_tls_registry_contains(&key),
            "entry must survive while a peer endpoint still shares the pair"
        );

        drop(higher);
        assert!(
            !loopback_tls_registry_contains(&key),
            "entry must be pruned once the last peer endpoint drops"
        );
    }
}
