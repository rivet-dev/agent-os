import { createBridgeSyncFacade } from "./fs.js";
import { exposeCustomGlobal } from "../global-exposure.js";
import { dgramModule } from "./dgram.js";

function isSqlitePlainObject(value) {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    return false;
  }
  if (Buffer.isBuffer(value) || value instanceof Uint8Array) {
    return false;
  }
  const prototype = Object.getPrototypeOf(value);
  return prototype === Object.prototype || prototype === null;
}

function encodeSqliteValue(value) {
  if (value === null || value === void 0 || typeof value === "boolean" || typeof value === "number" || typeof value === "string") {
    return value ?? null;
  }
  if (typeof value === "bigint") {
    return {
      __agentosSqliteType: "bigint",
      value: value.toString()
    };
  }
  if (Buffer.isBuffer(value) || value instanceof Uint8Array) {
    return {
      __agentosSqliteType: "uint8array",
      value: Buffer.from(value).toString("base64")
    };
  }
  if (Array.isArray(value)) {
    return value.map((entry) => encodeSqliteValue(entry));
  }
  if (value && typeof value === "object") {
    return Object.fromEntries(
      Object.entries(value).map(([key, entry]) => [key, encodeSqliteValue(entry)])
    );
  }
  return null;
}

function decodeSqliteValue(value) {
  if (value === null || value === void 0 || typeof value === "boolean" || typeof value === "number" || typeof value === "string") {
    return value ?? null;
  }
  if (Array.isArray(value)) {
    return value.map((entry) => decodeSqliteValue(entry));
  }
  if (value && typeof value === "object") {
    if (value.__agentosSqliteType === "bigint" && typeof value.value === "string") {
      return BigInt(value.value);
    }
    if (value.__agentosSqliteType === "uint8array" && typeof value.value === "string") {
      return Buffer.from(value.value, "base64");
    }
    return Object.fromEntries(
      Object.entries(value).map(([key, entry]) => [key, decodeSqliteValue(entry)])
    );
  }
  return value;
}

function normalizeSqliteParams(params) {
  if (!Array.isArray(params) || params.length === 0) {
    return null;
  }
  if (params.length === 1 && isSqlitePlainObject(params[0])) {
    return encodeSqliteValue(params[0]);
  }
  return params.map((entry) => encodeSqliteValue(entry));
}

function sqliteBridgeCall(bridgeFn, args, label) {
  if (typeof bridgeFn === "function") {
    return decodeSqliteValue(bridgeFn(...args));
  }
  if (!bridgeFn) {
    throw new Error(`sqlite bridge is not available for ${label}`);
  }
  if (typeof bridgeFn.applySync === "function") {
    return decodeSqliteValue(bridgeFn.applySync(void 0, args));
  }
  if (typeof bridgeFn.applySyncPromise === "function") {
    return decodeSqliteValue(bridgeFn.applySyncPromise(void 0, args));
  }
  throw new Error(`sqlite bridge is not available for ${label}`);
}

var _sqliteConstants = createBridgeSyncFacade("_sqliteConstantsRaw");

var _sqliteDatabaseOpen = createBridgeSyncFacade("_sqliteDatabaseOpenRaw");

var _sqliteDatabaseClose = createBridgeSyncFacade("_sqliteDatabaseCloseRaw");

var _sqliteDatabaseExec = createBridgeSyncFacade("_sqliteDatabaseExecRaw");

var _sqliteDatabaseQuery = createBridgeSyncFacade("_sqliteDatabaseQueryRaw");

var _sqliteDatabasePrepare = createBridgeSyncFacade("_sqliteDatabasePrepareRaw");

var _sqliteDatabaseLocation = createBridgeSyncFacade("_sqliteDatabaseLocationRaw");

var _sqliteDatabaseCheckpoint = createBridgeSyncFacade("_sqliteDatabaseCheckpointRaw");

var _sqliteStatementRun = createBridgeSyncFacade("_sqliteStatementRunRaw");

var _sqliteStatementGet = createBridgeSyncFacade("_sqliteStatementGetRaw");

var _sqliteStatementAll = createBridgeSyncFacade("_sqliteStatementAllRaw");

var _sqliteStatementColumns = createBridgeSyncFacade("_sqliteStatementColumnsRaw");

var _sqliteStatementSetReturnArrays = createBridgeSyncFacade("_sqliteStatementSetReturnArraysRaw");

var _sqliteStatementSetReadBigInts = createBridgeSyncFacade("_sqliteStatementSetReadBigIntsRaw");

var _sqliteStatementSetAllowBareNamedParameters = createBridgeSyncFacade("_sqliteStatementSetAllowBareNamedParametersRaw");

var _sqliteStatementSetAllowUnknownNamedParameters = createBridgeSyncFacade("_sqliteStatementSetAllowUnknownNamedParametersRaw");

var _sqliteStatementFinalize = createBridgeSyncFacade("_sqliteStatementFinalizeRaw");

var sqliteStatementFinalizer = typeof FinalizationRegistry === "function"
  ? new FinalizationRegistry((statementId) => {
      try {
        sqliteBridgeCall(
          _sqliteStatementFinalize,
          [statementId],
          "statement garbage collection"
        );
      } catch {
        // Database close and process teardown also release statement handles.
      }
    })
  : null;

var StatementSync = class {
  constructor(database, sql, statementId) {
    this._database = database;
    this._sql = sql;
    this._statementId = statementId;
    this._finalized = false;
    this._returnArrays = false;
    this._readBigInts = false;
    this._allowBareNamedParameters = false;
    this._allowUnknownNamedParameters = false;
    sqliteStatementFinalizer?.register(this, statementId, this);
  }
  _assertOpen() {
    this._database._assertOpen();
    if (this._finalized) {
      throw new Error("SQLite statement is already finalized");
    }
  }
  _ensureStatement() {
    this._assertOpen();
    if (this._statementId !== null) return this._statementId;
    const statementId = sqliteBridgeCall(
      _sqliteDatabasePrepare,
      [this._database._databaseId, this._sql],
      "database.prepare"
    );
    this._statementId = statementId;
    sqliteStatementFinalizer?.register(this, statementId, this);
    if (this._returnArrays) {
      sqliteBridgeCall(_sqliteStatementSetReturnArrays, [statementId, true], "statement.setReturnArrays");
    }
    if (this._readBigInts) {
      sqliteBridgeCall(_sqliteStatementSetReadBigInts, [statementId, true], "statement.setReadBigInts");
    }
    if (this._allowBareNamedParameters) {
      sqliteBridgeCall(_sqliteStatementSetAllowBareNamedParameters, [statementId, true], "statement.setAllowBareNamedParameters");
    }
    if (this._allowUnknownNamedParameters) {
      sqliteBridgeCall(_sqliteStatementSetAllowUnknownNamedParameters, [statementId, true], "statement.setAllowUnknownNamedParameters");
    }
    return statementId;
  }
  _releaseStatement(suppressErrors = false) {
    if (this._statementId === null) return;
    const statementId = this._statementId;
    this._statementId = null;
    sqliteStatementFinalizer?.unregister(this);
    try {
      sqliteBridgeCall(
        _sqliteStatementFinalize,
        [statementId],
        "statement.finalize"
      );
    } catch (error) {
      if (!suppressErrors) throw error;
    }
  }
  _execute(bridgeFn, args, label) {
    const statementId = this._ensureStatement();
    let failed = false;
    try {
      return sqliteBridgeCall(bridgeFn, [statementId, ...args], label);
    } catch (error) {
      failed = true;
      throw error;
    } finally {
      this._releaseStatement(failed);
    }
  }
  run(...params) {
    return this._execute(
      _sqliteStatementRun,
      [normalizeSqliteParams(params)],
      "statement.run"
    );
  }
  get(...params) {
    return this._execute(
      _sqliteStatementGet,
      [normalizeSqliteParams(params)],
      "statement.get"
    );
  }
  all(...params) {
    return this._execute(
      _sqliteStatementAll,
      [normalizeSqliteParams(params)],
      "statement.all"
    );
  }
  iterate(...params) {
    const rows = this.all(...params);
    return rows[Symbol.iterator]();
  }
  columns() {
    return this._execute(
      _sqliteStatementColumns,
      [],
      "statement.columns"
    );
  }
  setReturnArrays(enabled) {
    this._assertOpen();
    this._returnArrays = Boolean(enabled);
    if (this._statementId !== null) {
      sqliteBridgeCall(
        _sqliteStatementSetReturnArrays,
        [this._statementId, this._returnArrays],
        "statement.setReturnArrays"
      );
    }
  }
  setReadBigInts(enabled) {
    this._assertOpen();
    this._readBigInts = Boolean(enabled);
    if (this._statementId !== null) {
      sqliteBridgeCall(
        _sqliteStatementSetReadBigInts,
        [this._statementId, this._readBigInts],
        "statement.setReadBigInts"
      );
    }
  }
  setAllowBareNamedParameters(enabled) {
    this._assertOpen();
    this._allowBareNamedParameters = Boolean(enabled);
    if (this._statementId !== null) {
      sqliteBridgeCall(
        _sqliteStatementSetAllowBareNamedParameters,
        [this._statementId, this._allowBareNamedParameters],
        "statement.setAllowBareNamedParameters"
      );
    }
  }
  setAllowUnknownNamedParameters(enabled) {
    this._assertOpen();
    this._allowUnknownNamedParameters = Boolean(enabled);
    if (this._statementId !== null) {
      sqliteBridgeCall(
        _sqliteStatementSetAllowUnknownNamedParameters,
        [this._statementId, this._allowUnknownNamedParameters],
        "statement.setAllowUnknownNamedParameters"
      );
    }
  }
  finalize() {
    if (this._finalized) {
      return null;
    }
    this._database._assertOpen();
    this._releaseStatement();
    this._finalized = true;
    return null;
  }
};

var DatabaseSync = class {
  constructor(location = ":memory:", options = void 0) {
    this._closed = false;
    this._databaseId = sqliteBridgeCall(
      _sqliteDatabaseOpen,
      [typeof location === "string" ? location : ":memory:", options ?? null],
      "database.open"
    );
  }
  _assertOpen() {
    if (this._closed) {
      throw new Error("SQLite database is already closed");
    }
  }
  close() {
    if (this._closed) {
      return null;
    }
    sqliteBridgeCall(
      _sqliteDatabaseClose,
      [this._databaseId],
      "database.close"
    );
    this._closed = true;
    return null;
  }
  exec(sql) {
    this._assertOpen();
    return sqliteBridgeCall(
      _sqliteDatabaseExec,
      [this._databaseId, String(sql ?? "")],
      "database.exec"
    );
  }
  query(sql, params = null, options = null) {
    this._assertOpen();
    const normalized = params === null ? null : normalizeSqliteParams(Array.isArray(params) ? params : [params]);
    return sqliteBridgeCall(
      _sqliteDatabaseQuery,
      [this._databaseId, String(sql ?? ""), normalized, options ?? null],
      "database.query"
    );
  }
  prepare(sql) {
    this._assertOpen();
    const normalizedSql = String(sql ?? "");
    const statementId = sqliteBridgeCall(
      _sqliteDatabasePrepare,
      [this._databaseId, normalizedSql],
      "database.prepare"
    );
    return new StatementSync(this, normalizedSql, statementId);
  }
  location() {
    this._assertOpen();
    return sqliteBridgeCall(
      _sqliteDatabaseLocation,
      [this._databaseId],
      "database.location"
    );
  }
  checkpoint() {
    this._assertOpen();
    return sqliteBridgeCall(
      _sqliteDatabaseCheckpoint,
      [this._databaseId],
      "database.checkpoint"
    );
  }
};

DatabaseSync.prototype[Symbol.dispose] = DatabaseSync.prototype.close;

StatementSync.prototype[Symbol.dispose] = StatementSync.prototype.finalize;

var sqliteConstants;

function getSqliteConstants() {
  if (sqliteConstants === void 0) {
    sqliteConstants = Object.freeze(
      sqliteBridgeCall(_sqliteConstants, [], "constants") ?? {}
    );
  }
  return sqliteConstants;
}

var sqliteModule = {
  DatabaseSync,
  StatementSync,
  get constants() {
    return getSqliteConstants();
  }
};

exposeCustomGlobal("_dgramModule", dgramModule);
exposeCustomGlobal("_sqliteModule", sqliteModule);
export { DatabaseSync, StatementSync, _sqliteConstants, _sqliteDatabaseCheckpoint, _sqliteDatabaseClose, _sqliteDatabaseExec, _sqliteDatabaseLocation, _sqliteDatabaseOpen, _sqliteDatabasePrepare, _sqliteDatabaseQuery, _sqliteStatementAll, _sqliteStatementColumns, _sqliteStatementFinalize, _sqliteStatementGet, _sqliteStatementRun, _sqliteStatementSetAllowBareNamedParameters, _sqliteStatementSetAllowUnknownNamedParameters, _sqliteStatementSetReadBigInts, _sqliteStatementSetReturnArrays, decodeSqliteValue, encodeSqliteValue, getSqliteConstants, isSqlitePlainObject, normalizeSqliteParams, sqliteBridgeCall, sqliteConstants, sqliteModule };
