/*
 * WASI TLS backend for libcurl
 *
 * Uses the host_net WASM import net_tls_connect() to upgrade a TCP
 * socket to TLS. The host runtime (Node.js tls.connect()) handles all
 * cryptographic operations transparently — after the upgrade, regular
 * send()/recv() carry encrypted traffic. This backend therefore just
 * calls the host import during connect and passes data through to the
 * socket layer for send/recv.
 */

#include "curl_setup.h"

#ifdef USE_WASI_TLS

#include <unistd.h>

#include "urldata.h"
#include "sendf.h"
#include "vtls.h"
#include "vtls_int.h"
#include "connect.h"
#include "select.h"
#include "curl_printf.h"

/* The last #include files should be: */
#include "curl_memory.h"
#include "memdebug.h"

/* WASM import: upgrade a TCP socket FD to TLS.
 * flags: 0 = verify peer certificate (default)
 *        1 = skip peer certificate verification (-k / --insecure)
 * Returns 0 on success, errno on failure. */
__attribute__((import_module("host_net"), import_name("net_tls_connect")))
extern int __wasi_net_tls_connect(int fd, const char *hostname_ptr,
                                  int hostname_len, int flags);

/* Per-connection backend data (minimal — host handles all TLS state) */
struct wasi_tls_backend_data {
  int dummy; /* struct must be non-empty */
};

static size_t wasi_tls_version(char *buffer, size_t size)
{
  return msnprintf(buffer, size, "WASI-TLS/host");
}

static CURLcode wasi_tls_connect_blocking(struct Curl_cfilter *cf,
                                          struct Curl_easy *data)
{
  struct ssl_connect_data *connssl = cf->ctx;
  curl_socket_t sockfd = Curl_conn_cf_get_socket(cf, data);
  const char *hostname = connssl->peer.hostname;
  int flags = 0;

  /* Check if peer verification is disabled (curl -k / --insecure) */
  struct ssl_primary_config *conn_config = Curl_ssl_cf_get_primary_config(cf);
  if(conn_config && !conn_config->verifypeer) {
    flags = 1;
  }

  int ret = __wasi_net_tls_connect((int)sockfd, hostname,
                                   (int)strlen(hostname), flags);
  if(ret != 0) {
    failf(data, "WASI TLS: host TLS connect failed (errno %d)", ret);
    return CURLE_SSL_CONNECT_ERROR;
  }

  connssl->state = ssl_connection_complete;
  return CURLE_OK;
}

static CURLcode wasi_tls_connect_nonblocking(struct Curl_cfilter *cf,
                                             struct Curl_easy *data,
                                             bool *done)
{
  /* Our host TLS connect is synchronous — just call blocking version */
  CURLcode result = wasi_tls_connect_blocking(cf, data);
  *done = (result == CURLE_OK);
  return result;
}

/* recv: pass through to the socket layer — host handles decryption */
static ssize_t wasi_tls_recv(struct Curl_cfilter *cf,
                             struct Curl_easy *data,
                             char *buf, size_t len, CURLcode *err)
{
  return Curl_conn_cf_recv(cf->next, data, buf, len, err);
}

/* send: pass through to the socket layer — host handles encryption */
static ssize_t wasi_tls_send(struct Curl_cfilter *cf,
                             struct Curl_easy *data,
                             const void *buf, size_t len, CURLcode *err)
{
  return Curl_conn_cf_send(cf->next, data, buf, len, FALSE, err);
}

static CURLcode wasi_tls_shutdown(struct Curl_cfilter *cf,
                                  struct Curl_easy *data,
                                  bool send_shutdown, bool *done)
{
  (void)cf;
  (void)data;
  (void)send_shutdown;
  *done = TRUE;
  return CURLE_OK;
}

static bool wasi_tls_data_pending(struct Curl_cfilter *cf,
                                  const struct Curl_easy *data)
{
  (void)cf;
  (void)data;
  return FALSE;
}

/* data may be NULL */
static CURLcode wasi_tls_random(struct Curl_easy *data,
                                unsigned char *entropy, size_t length)
{
  size_t offset = 0;
  (void)data;

  while(offset < length) {
    size_t chunk = length - offset;
    if(chunk > 256)
      chunk = 256;

    if(getentropy(entropy + offset, chunk) != 0)
      return CURLE_FAILED_INIT;

    offset += chunk;
  }

  return CURLE_OK;
}

static void wasi_tls_close(struct Curl_cfilter *cf, struct Curl_easy *data)
{
  (void)cf;
  (void)data;
  /* Host-side TLS is cleaned up when the socket is closed */
}

static void *wasi_tls_get_internals(struct ssl_connect_data *connssl,
                                    CURLINFO info)
{
  (void)connssl;
  (void)info;
  return NULL;
}

const struct Curl_ssl Curl_ssl_wasi_tls = {
  { CURLSSLBACKEND_NONE, "wasi-tls" },  /* info */

  0,  /* supports — no special features */

  sizeof(struct wasi_tls_backend_data),

  Curl_none_init,                    /* init */
  Curl_none_cleanup,                 /* cleanup */
  wasi_tls_version,                  /* version */
  Curl_none_check_cxn,              /* check_cxn */
  wasi_tls_shutdown,                 /* shutdown */
  wasi_tls_data_pending,             /* data_pending */
  wasi_tls_random,                   /* random */
  Curl_none_cert_status_request,     /* cert_status_request */
  wasi_tls_connect_blocking,         /* connect */
  wasi_tls_connect_nonblocking,      /* connect_nonblocking */
  Curl_ssl_adjust_pollset,           /* adjust_pollset */
  wasi_tls_get_internals,            /* get_internals */
  wasi_tls_close,                    /* close */
  Curl_none_close_all,               /* close_all */
  Curl_none_set_engine,              /* set_engine */
  Curl_none_set_engine_default,      /* set_engine_default */
  Curl_none_engines_list,            /* engines_list */
  Curl_none_false_start,             /* false_start */
  NULL,                              /* sha256sum */
  NULL,                              /* associate_connection */
  NULL,                              /* disassociate_connection */
  wasi_tls_recv,                     /* recv decrypted data */
  wasi_tls_send,                     /* send data to encrypt */
  NULL,                              /* get_channel_binding */
};

#endif /* USE_WASI_TLS */
