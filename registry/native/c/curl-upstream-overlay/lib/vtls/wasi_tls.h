#ifndef HEADER_CURL_WASI_TLS_H
#define HEADER_CURL_WASI_TLS_H
/*
 * WASI TLS backend for libcurl
 *
 * Delegates TLS to the host runtime via the host_net WASM import
 * net_tls_connect(). After the host upgrades the socket, regular
 * send()/recv() transparently carry encrypted traffic, so the
 * backend's recv_plain/send_plain are simple pass-throughs.
 */

#include "curl_setup.h"

#ifdef USE_WASI_TLS
extern const struct Curl_ssl Curl_ssl_wasi_tls;
#endif

#endif /* HEADER_CURL_WASI_TLS_H */
