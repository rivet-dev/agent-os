"use strict";

const constants = {
  HTTP2_HEADER_METHOD: ":method",
  HTTP2_HEADER_PATH: ":path",
  HTTP2_HEADER_SCHEME: ":scheme",
  HTTP2_HEADER_AUTHORITY: ":authority",
  HTTP2_HEADER_STATUS: ":status",
  HTTP2_HEADER_CONTENT_TYPE: "content-type",
  HTTP2_HEADER_CONTENT_LENGTH: "content-length",
  HTTP2_HEADER_LAST_MODIFIED: "last-modified",
  HTTP2_HEADER_ACCEPT: "accept",
  HTTP2_HEADER_ACCEPT_ENCODING: "accept-encoding",
  HTTP2_METHOD_GET: "GET",
  HTTP2_METHOD_POST: "POST",
  HTTP2_METHOD_PUT: "PUT",
  HTTP2_METHOD_DELETE: "DELETE",
  DEFAULT_SETTINGS_MAX_HEADER_LIST_SIZE: 65535,
};

function notImplemented(name) {
  const error = new Error(`node:http2 ${name} is not available in the Agent OS bridge bootstrap`);
  error.code = "ERR_NOT_IMPLEMENTED";
  throw error;
}

function connect() {
  notImplemented("connect");
}

function createServer() {
  notImplemented("createServer");
}

function createSecureServer() {
  notImplemented("createSecureServer");
}

function getDefaultSettings() {
  return {
    maxHeaderListSize: constants.DEFAULT_SETTINGS_MAX_HEADER_LIST_SIZE,
  };
}

module.exports = {
  constants,
  connect,
  createServer,
  createSecureServer,
  getDefaultSettings,
};
