use super::super::*;

const DEFAULT_SCRYPT_COST: u64 = 16_384;
const DEFAULT_SCRYPT_BLOCK_SIZE: u32 = 8;
const DEFAULT_SCRYPT_PARALLELIZATION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum JavascriptCryptoDigestAlgorithm {
    Md5,
    Sha1,
    Sha224,
    Sha256,
    Sha384,
    Sha512,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct JavascriptScryptOptions {
    #[serde(alias = "N")]
    cost: Option<u64>,
    #[serde(alias = "r")]
    block_size: Option<u32>,
    #[serde(alias = "p")]
    parallelization: Option<u32>,
}

pub(crate) fn service_javascript_crypto_sync_rpc(
    process: &mut ActiveProcess,
    request: &HostRpcRequest,
) -> Result<Value, SidecarError> {
    match request.method.as_str() {
        "crypto.hashDigest" => {
            let algorithm = javascript_crypto_digest_algorithm(
                &request.args,
                0,
                "crypto.hashDigest algorithm",
            )?;
            let data = javascript_sync_rpc_base64_arg(&request.args, 1, "crypto.hashDigest data")?;
            Ok(Value::String(
                base64::engine::general_purpose::STANDARD.encode(algorithm.digest(&data)),
            ))
        }
        "crypto.hashCreate" => service_javascript_crypto_hash_create_sync_rpc(process, request),
        "crypto.hashUpdate" => service_javascript_crypto_hash_update_sync_rpc(process, request),
        "crypto.hashFinal" => service_javascript_crypto_hash_final_sync_rpc(process, request),
        "crypto.hashDestroy" => {
            let session_id =
                javascript_sync_rpc_arg_u64(&request.args, 0, "crypto.hashDestroy session id")?;
            process.hash_sessions.remove(&session_id);
            Ok(Value::Null)
        }
        "crypto.hmacDigest" => {
            let algorithm = javascript_crypto_digest_algorithm(
                &request.args,
                0,
                "crypto.hmacDigest algorithm",
            )?;
            let key = javascript_sync_rpc_base64_arg(&request.args, 1, "crypto.hmacDigest key")?;
            let data = javascript_sync_rpc_base64_arg(&request.args, 2, "crypto.hmacDigest data")?;
            Ok(Value::String(
                base64::engine::general_purpose::STANDARD.encode(algorithm.hmac(&key, &data)?),
            ))
        }
        "crypto.pbkdf2" => {
            let password =
                javascript_sync_rpc_base64_arg(&request.args, 0, "crypto.pbkdf2 password")?;
            let salt = javascript_sync_rpc_base64_arg(&request.args, 1, "crypto.pbkdf2 salt")?;
            let iterations =
                javascript_sync_rpc_arg_u32(&request.args, 2, "crypto.pbkdf2 iterations")?;
            if iterations == 0 {
                return Err(SidecarError::InvalidState(String::from(
                    "crypto.pbkdf2 iterations must be greater than zero",
                )));
            }
            let key_len = usize::try_from(javascript_sync_rpc_arg_u64(
                &request.args,
                3,
                "crypto.pbkdf2 key length",
            )?)
            .map_err(|_| {
                SidecarError::InvalidState(String::from(
                    "crypto.pbkdf2 key length must fit within usize",
                ))
            })?;
            let algorithm =
                javascript_crypto_digest_algorithm(&request.args, 4, "crypto.pbkdf2 digest")?;
            let mut output = vec![0u8; key_len];
            algorithm.pbkdf2(&password, &salt, iterations, &mut output);
            Ok(Value::String(
                base64::engine::general_purpose::STANDARD.encode(output),
            ))
        }
        "crypto.scrypt" => {
            let password =
                javascript_sync_rpc_base64_arg(&request.args, 0, "crypto.scrypt password")?;
            let salt = javascript_sync_rpc_base64_arg(&request.args, 1, "crypto.scrypt salt")?;
            let key_len = usize::try_from(javascript_sync_rpc_arg_u64(
                &request.args,
                2,
                "crypto.scrypt key length",
            )?)
            .map_err(|_| {
                SidecarError::InvalidState(String::from(
                    "crypto.scrypt key length must fit within usize",
                ))
            })?;
            let options_json =
                javascript_sync_rpc_arg_str(&request.args, 3, "crypto.scrypt options")?;
            let options: JavascriptScryptOptions =
                serde_json::from_str(options_json).map_err(|error| {
                    SidecarError::InvalidState(format!(
                        "crypto.scrypt options must be valid JSON: {error}"
                    ))
                })?;
            let cost = options.cost.unwrap_or(DEFAULT_SCRYPT_COST);
            if cost == 0 || !cost.is_power_of_two() {
                return Err(SidecarError::InvalidState(String::from(
                    "crypto.scrypt cost must be a positive power of two",
                )));
            }
            let log_n = u8::try_from(cost.ilog2()).map_err(|_| {
                SidecarError::InvalidState(String::from(
                    "crypto.scrypt cost exceeds supported parameter range",
                ))
            })?;
            let params = ScryptParams::new(
                log_n,
                options.block_size.unwrap_or(DEFAULT_SCRYPT_BLOCK_SIZE),
                options
                    .parallelization
                    .unwrap_or(DEFAULT_SCRYPT_PARALLELIZATION),
                key_len,
            )
            .map_err(|error| {
                SidecarError::InvalidState(format!("crypto.scrypt options are invalid: {error}"))
            })?;
            let mut output = vec![0u8; key_len];
            scrypt(&password, &salt, &params, &mut output).map_err(|error| {
                SidecarError::Execution(format!("crypto.scrypt failed: {error}"))
            })?;
            Ok(Value::String(
                base64::engine::general_purpose::STANDARD.encode(output),
            ))
        }
        "crypto.cipheriv" => service_javascript_crypto_cipheriv_sync_rpc(request),
        "crypto.decipheriv" => service_javascript_crypto_decipheriv_sync_rpc(request),
        "crypto.cipherivCreate" => {
            service_javascript_crypto_cipheriv_create_sync_rpc(process, request)
        }
        "crypto.cipherivUpdate" => {
            service_javascript_crypto_cipheriv_update_sync_rpc(process, request)
        }
        "crypto.cipherivFinal" => {
            service_javascript_crypto_cipheriv_final_sync_rpc(process, request)
        }
        "crypto.sign" => service_javascript_crypto_sign_sync_rpc(request),
        "crypto.verify" => service_javascript_crypto_verify_sync_rpc(request),
        "crypto.asymmetricOp" => service_javascript_crypto_asymmetric_op_sync_rpc(request),
        "crypto.createKeyObject" => service_javascript_crypto_create_key_object_sync_rpc(request),
        "crypto.generateKeyPairSync" => {
            service_javascript_crypto_generate_key_pair_sync_rpc(request)
        }
        "crypto.generateKeySync" => service_javascript_crypto_generate_key_sync_rpc(request),
        "crypto.generatePrimeSync" => service_javascript_crypto_generate_prime_sync_rpc(request),
        "crypto.diffieHellman" => service_javascript_crypto_diffie_hellman_sync_rpc(request),
        "crypto.diffieHellmanGroup" => {
            service_javascript_crypto_diffie_hellman_group_sync_rpc(request)
        }
        "crypto.diffieHellmanSessionCreate" => {
            service_javascript_crypto_diffie_hellman_session_create_sync_rpc(process, request)
        }
        "crypto.diffieHellmanSessionCall" => {
            service_javascript_crypto_diffie_hellman_session_call_sync_rpc(process, request)
        }
        "crypto.diffieHellmanSessionDestroy" => {
            service_javascript_crypto_diffie_hellman_session_destroy_sync_rpc(process, request)
        }
        "crypto.subtle" => service_javascript_crypto_subtle_sync_rpc(request),
        _ => Err(SidecarError::InvalidState(format!(
            "unsupported JavaScript crypto sync RPC method {}",
            request.method
        ))),
    }
}

fn service_javascript_crypto_hash_create_sync_rpc(
    process: &mut ActiveProcess,
    request: &HostRpcRequest,
) -> Result<Value, SidecarError> {
    ensure_per_process_state_handle_capacity(process.hash_sessions.len(), "hash session")?;
    let algorithm =
        javascript_crypto_digest_algorithm(&request.args, 0, "crypto.hashCreate algorithm")?;
    let context = openssl::hash::Hasher::new(algorithm.message_digest()).map_err(|error| {
        SidecarError::InvalidState(format!("failed to create crypto hash session: {error}"))
    })?;
    process.next_hash_session_id += 1;
    let session_id = process.next_hash_session_id;
    process
        .hash_sessions
        .insert(session_id, ActiveHashSession { context });
    Ok(json!(session_id))
}

fn service_javascript_crypto_hash_update_sync_rpc(
    process: &mut ActiveProcess,
    request: &HostRpcRequest,
) -> Result<Value, SidecarError> {
    let session_id = javascript_sync_rpc_arg_u64(&request.args, 0, "crypto.hashUpdate session id")?;
    let data = request
        .raw_bytes_args
        .get(&1)
        .cloned()
        .map(Ok)
        .unwrap_or_else(|| {
            javascript_sync_rpc_bytes_arg(&request.args, 1, "crypto.hashUpdate data")
        })?;
    let session = process.hash_sessions.get_mut(&session_id).ok_or_else(|| {
        SidecarError::InvalidState(format!("Hash session {session_id} not found"))
    })?;
    session.context.update(&data).map_err(|error| {
        SidecarError::InvalidState(format!("failed to update crypto hash session: {error}"))
    })?;
    Ok(Value::Null)
}

fn service_javascript_crypto_hash_final_sync_rpc(
    process: &mut ActiveProcess,
    request: &HostRpcRequest,
) -> Result<Value, SidecarError> {
    let session_id = javascript_sync_rpc_arg_u64(&request.args, 0, "crypto.hashFinal session id")?;
    let mut session = process.hash_sessions.remove(&session_id).ok_or_else(|| {
        SidecarError::InvalidState(format!("Hash session {session_id} not found"))
    })?;
    let digest = session.context.finish().map_err(|error| {
        SidecarError::InvalidState(format!("failed to finish crypto hash session: {error}"))
    })?;
    Ok(Value::String(
        base64::engine::general_purpose::STANDARD.encode(digest),
    ))
}

fn javascript_crypto_digest_algorithm(
    args: &[Value],
    index: usize,
    label: &str,
) -> Result<JavascriptCryptoDigestAlgorithm, SidecarError> {
    JavascriptCryptoDigestAlgorithm::parse(javascript_sync_rpc_arg_str(args, index, label)?)
}

impl JavascriptCryptoDigestAlgorithm {
    pub(in crate::execution) fn parse(value: &str) -> Result<Self, SidecarError> {
        match value.trim().to_ascii_lowercase().replace('-', "").as_str() {
            "md5" => Ok(Self::Md5),
            "sha1" => Ok(Self::Sha1),
            "sha224" => Ok(Self::Sha224),
            "sha256" => Ok(Self::Sha256),
            "sha384" => Ok(Self::Sha384),
            "sha512" => Ok(Self::Sha512),
            _ => Err(SidecarError::InvalidState(format!(
                "unsupported crypto digest algorithm {value}"
            ))),
        }
    }

    pub(in crate::execution) fn digest(self, data: &[u8]) -> Vec<u8> {
        match self {
            Self::Md5 => Md5::digest(data).to_vec(),
            Self::Sha1 => Sha1::digest(data).to_vec(),
            Self::Sha224 => Sha224::digest(data).to_vec(),
            Self::Sha256 => Sha256::digest(data).to_vec(),
            Self::Sha384 => Sha384::digest(data).to_vec(),
            Self::Sha512 => Sha512::digest(data).to_vec(),
        }
    }

    fn message_digest(self) -> MessageDigest {
        match self {
            Self::Md5 => MessageDigest::md5(),
            Self::Sha1 => MessageDigest::sha1(),
            Self::Sha224 => MessageDigest::sha224(),
            Self::Sha256 => MessageDigest::sha256(),
            Self::Sha384 => MessageDigest::sha384(),
            Self::Sha512 => MessageDigest::sha512(),
        }
    }

    fn hmac(self, key: &[u8], data: &[u8]) -> Result<Vec<u8>, SidecarError> {
        match self {
            Self::Md5 => {
                let mut mac = Hmac::<Md5>::new_from_slice(key).map_err(|error| {
                    SidecarError::InvalidState(format!("invalid HMAC key: {error}"))
                })?;
                mac.update(data);
                Ok(mac.finalize().into_bytes().to_vec())
            }
            Self::Sha1 => {
                let mut mac = Hmac::<Sha1>::new_from_slice(key).map_err(|error| {
                    SidecarError::InvalidState(format!("invalid HMAC key: {error}"))
                })?;
                mac.update(data);
                Ok(mac.finalize().into_bytes().to_vec())
            }
            Self::Sha224 => {
                let mut mac = Hmac::<Sha224>::new_from_slice(key).map_err(|error| {
                    SidecarError::InvalidState(format!("invalid HMAC key: {error}"))
                })?;
                mac.update(data);
                Ok(mac.finalize().into_bytes().to_vec())
            }
            Self::Sha256 => {
                let mut mac = Hmac::<Sha256>::new_from_slice(key).map_err(|error| {
                    SidecarError::InvalidState(format!("invalid HMAC key: {error}"))
                })?;
                mac.update(data);
                Ok(mac.finalize().into_bytes().to_vec())
            }
            Self::Sha384 => {
                let mut mac = Hmac::<Sha384>::new_from_slice(key).map_err(|error| {
                    SidecarError::InvalidState(format!("invalid HMAC key: {error}"))
                })?;
                mac.update(data);
                Ok(mac.finalize().into_bytes().to_vec())
            }
            Self::Sha512 => {
                let mut mac = Hmac::<Sha512>::new_from_slice(key).map_err(|error| {
                    SidecarError::InvalidState(format!("invalid HMAC key: {error}"))
                })?;
                mac.update(data);
                Ok(mac.finalize().into_bytes().to_vec())
            }
        }
    }

    pub(in crate::execution) fn pbkdf2(
        self,
        password: &[u8],
        salt: &[u8],
        iterations: u32,
        output: &mut [u8],
    ) {
        match self {
            Self::Md5 => pbkdf2_hmac::<Md5>(password, salt, iterations, output),
            Self::Sha1 => pbkdf2_hmac::<Sha1>(password, salt, iterations, output),
            Self::Sha224 => pbkdf2_hmac::<Sha224>(password, salt, iterations, output),
            Self::Sha256 => pbkdf2_hmac::<Sha256>(password, salt, iterations, output),
            Self::Sha384 => pbkdf2_hmac::<Sha384>(password, salt, iterations, output),
            Self::Sha512 => pbkdf2_hmac::<Sha512>(password, salt, iterations, output),
        }
    }
}

#[derive(Debug, Clone)]
enum JavascriptCryptoKeyMaterial {
    Private(PKey<Private>),
    Public(PKey<Public>),
    Secret(Vec<u8>),
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct JavascriptSerializedSandboxKeyObject {
    #[serde(rename = "type")]
    kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pem: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    raw: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "asymmetricKeyType")]
    asymmetric_key_type: Option<String>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        rename = "asymmetricKeyDetails"
    )]
    asymmetric_key_details: Option<Map<String, Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    jwk: Option<Value>,
}

#[derive(Debug, Clone)]
struct JavascriptDirectKeyInput {
    key: JavascriptCryptoKeyMaterial,
    padding: Option<Padding>,
}

fn service_javascript_crypto_cipheriv_sync_rpc(
    request: &HostRpcRequest,
) -> Result<Value, SidecarError> {
    service_javascript_crypto_cipheriv_inner(request, false)
}

fn service_javascript_crypto_decipheriv_sync_rpc(
    request: &HostRpcRequest,
) -> Result<Value, SidecarError> {
    service_javascript_crypto_cipheriv_inner(request, true)
}

fn service_javascript_crypto_cipheriv_create_sync_rpc(
    process: &mut ActiveProcess,
    request: &HostRpcRequest,
) -> Result<Value, SidecarError> {
    ensure_per_process_state_handle_capacity(process.cipher_sessions.len(), "cipher session")?;
    let mode = javascript_sync_rpc_arg_str(&request.args, 0, "crypto.cipherivCreate mode")?;
    let decrypt = mode == "decipher";
    let algorithm =
        javascript_sync_rpc_arg_str(&request.args, 1, "crypto.cipherivCreate algorithm")?;
    let key = javascript_sync_rpc_base64_arg(&request.args, 2, "crypto.cipherivCreate key")?;
    let iv = javascript_sync_rpc_base64_arg_optional(&request.args, 3, "crypto.cipherivCreate iv")?;
    let options =
        javascript_sync_rpc_json_arg_optional(&request.args, 4, "crypto.cipherivCreate options")?;
    let context = javascript_crypto_build_cipher_session(
        algorithm,
        &key,
        iv.as_deref(),
        decrypt,
        options.as_ref(),
    )?;
    process.next_cipher_session_id += 1;
    let session_id = process.next_cipher_session_id;
    process
        .cipher_sessions
        .insert(session_id, ActiveCipherSession { context });
    Ok(json!(session_id))
}

fn service_javascript_crypto_cipheriv_update_sync_rpc(
    process: &mut ActiveProcess,
    request: &HostRpcRequest,
) -> Result<Value, SidecarError> {
    let session_id =
        javascript_sync_rpc_arg_u64(&request.args, 0, "crypto.cipherivUpdate session id")?;
    let data = javascript_sync_rpc_base64_arg(&request.args, 1, "crypto.cipherivUpdate data")?;
    let session = process
        .cipher_sessions
        .get_mut(&session_id)
        .ok_or_else(|| {
            SidecarError::InvalidState(format!("Cipher session {session_id} not found"))
        })?;
    let result = javascript_crypto_cipher_update(&mut session.context, &data)?;
    Ok(Value::String(
        base64::engine::general_purpose::STANDARD.encode(result),
    ))
}

fn service_javascript_crypto_cipheriv_final_sync_rpc(
    process: &mut ActiveProcess,
    request: &HostRpcRequest,
) -> Result<Value, SidecarError> {
    let session_id =
        javascript_sync_rpc_arg_u64(&request.args, 0, "crypto.cipherivFinal session id")?;
    let session = process.cipher_sessions.remove(&session_id).ok_or_else(|| {
        SidecarError::InvalidState(format!("Cipher session {session_id} not found"))
    })?;
    let outcome = session
        .context
        .finalize()
        .map_err(javascript_crypto_cipher_error)?;
    let mut response = Map::new();
    response.insert(
        String::from("data"),
        Value::String(base64::engine::general_purpose::STANDARD.encode(outcome.data)),
    );
    if let Some(auth_tag) = outcome.auth_tag {
        response.insert(
            String::from("authTag"),
            Value::String(base64::engine::general_purpose::STANDARD.encode(auth_tag)),
        );
    }
    Ok(Value::String(serde_json::to_string(&response).map_err(
        |error| SidecarError::InvalidState(format!("serialize cipher final response: {error}")),
    )?))
}

fn service_javascript_crypto_sign_sync_rpc(
    request: &HostRpcRequest,
) -> Result<Value, SidecarError> {
    let algorithm = request.args.first().and_then(Value::as_str);
    let data = javascript_sync_rpc_base64_arg(&request.args, 1, "crypto.sign data")?;
    let key_json = javascript_sync_rpc_arg_str(&request.args, 2, "crypto.sign key")?;
    let key_input =
        javascript_crypto_parse_direct_key_input(key_json, Some("private"), "crypto.sign key")?;
    let private_key = javascript_crypto_expect_private_key(key_input.key, "crypto.sign key")?;
    let mut signer = javascript_crypto_new_signer(algorithm, &private_key)?;
    if let Some(padding) = key_input.padding {
        signer
            .set_rsa_padding(padding)
            .map_err(javascript_crypto_openssl_error)?;
    }
    signer
        .update(&data)
        .map_err(javascript_crypto_openssl_error)?;
    Ok(Value::String(
        base64::engine::general_purpose::STANDARD.encode(
            signer
                .sign_to_vec()
                .map_err(javascript_crypto_openssl_error)?,
        ),
    ))
}

fn service_javascript_crypto_verify_sync_rpc(
    request: &HostRpcRequest,
) -> Result<Value, SidecarError> {
    let algorithm = request.args.first().and_then(Value::as_str);
    let data = javascript_sync_rpc_base64_arg(&request.args, 1, "crypto.verify data")?;
    let key_json = javascript_sync_rpc_arg_str(&request.args, 2, "crypto.verify key")?;
    let signature = javascript_sync_rpc_base64_arg(&request.args, 3, "crypto.verify signature")?;
    let key_input =
        javascript_crypto_parse_direct_key_input(key_json, Some("public"), "crypto.verify key")?;
    let public_key = javascript_crypto_expect_public_key(key_input.key, "crypto.verify key")?;
    let mut verifier = javascript_crypto_new_verifier(algorithm, &public_key)?;
    if let Some(padding) = key_input.padding {
        verifier
            .set_rsa_padding(padding)
            .map_err(javascript_crypto_openssl_error)?;
    }
    verifier
        .update(&data)
        .map_err(javascript_crypto_openssl_error)?;
    Ok(json!(verifier
        .verify(&signature)
        .map_err(javascript_crypto_openssl_error)?))
}

fn service_javascript_crypto_asymmetric_op_sync_rpc(
    request: &HostRpcRequest,
) -> Result<Value, SidecarError> {
    let operation = javascript_sync_rpc_arg_str(&request.args, 0, "crypto.asymmetricOp operation")?;
    let key_json = javascript_sync_rpc_arg_str(&request.args, 1, "crypto.asymmetricOp key")?;
    let data = javascript_sync_rpc_base64_arg(&request.args, 2, "crypto.asymmetricOp data")?;
    let expect_kind = match operation {
        "publicEncrypt" | "publicDecrypt" => Some("public"),
        "privateEncrypt" | "privateDecrypt" => Some("private"),
        other => {
            return Err(SidecarError::InvalidState(format!(
                "Unsupported asymmetric crypto operation: {other}"
            )));
        }
    };
    let key_input =
        javascript_crypto_parse_direct_key_input(key_json, expect_kind, "crypto.asymmetricOp key")?;
    let padding = key_input.padding.unwrap_or(Padding::PKCS1);
    let mut output = vec![0_u8; javascript_crypto_rsa_output_size(&key_input.key)?];
    let written = match (operation, key_input.key) {
        ("publicEncrypt", JavascriptCryptoKeyMaterial::Public(key))
        | ("publicDecrypt", JavascriptCryptoKeyMaterial::Public(key)) => {
            let rsa = key.rsa().map_err(javascript_crypto_openssl_error)?;
            if operation == "publicEncrypt" {
                rsa.public_encrypt(&data, &mut output, padding)
                    .map_err(javascript_crypto_openssl_error)?
            } else {
                rsa.public_decrypt(&data, &mut output, padding)
                    .map_err(javascript_crypto_openssl_error)?
            }
        }
        ("privateEncrypt", JavascriptCryptoKeyMaterial::Private(key))
        | ("privateDecrypt", JavascriptCryptoKeyMaterial::Private(key)) => {
            let rsa = key.rsa().map_err(javascript_crypto_openssl_error)?;
            if operation == "privateEncrypt" {
                rsa.private_encrypt(&data, &mut output, padding)
                    .map_err(javascript_crypto_openssl_error)?
            } else {
                rsa.private_decrypt(&data, &mut output, padding)
                    .map_err(javascript_crypto_openssl_error)?
            }
        }
        _ => {
            return Err(SidecarError::InvalidState(format!(
                "{operation} requires an RSA {} key",
                expect_kind.unwrap_or("asymmetric")
            )));
        }
    };
    output.truncate(written);
    Ok(Value::String(
        base64::engine::general_purpose::STANDARD.encode(output),
    ))
}

fn service_javascript_crypto_create_key_object_sync_rpc(
    request: &HostRpcRequest,
) -> Result<Value, SidecarError> {
    let operation =
        javascript_sync_rpc_arg_str(&request.args, 0, "crypto.createKeyObject operation")?;
    let key_json = javascript_sync_rpc_arg_str(&request.args, 1, "crypto.createKeyObject key")?;
    let expected = match operation {
        "createPrivateKey" => Some("private"),
        "createPublicKey" => Some("public"),
        other => {
            return Err(SidecarError::InvalidState(format!(
                "Unsupported key creation operation: {other}"
            )));
        }
    };
    let key_input =
        javascript_crypto_parse_direct_key_input(key_json, expected, "crypto.createKeyObject key")?;
    Ok(Value::String(
        serde_json::to_string(&javascript_crypto_serialize_sandbox_key_object(
            &key_input.key,
        )?)
        .map_err(|error| {
            SidecarError::InvalidState(format!("serialize crypto key object: {error}"))
        })?,
    ))
}

fn service_javascript_crypto_generate_key_pair_sync_rpc(
    request: &HostRpcRequest,
) -> Result<Value, SidecarError> {
    let key_type =
        javascript_sync_rpc_arg_str(&request.args, 0, "crypto.generateKeyPairSync type")?;
    let options = javascript_crypto_parse_serialized_options_arg(
        &request.args,
        1,
        "crypto.generateKeyPairSync options",
    )?
    .unwrap_or(Value::Object(Map::new()));
    let public_encoding = options.get("publicKeyEncoding").cloned();
    let private_encoding = options.get("privateKeyEncoding").cloned();

    let private_key = match key_type {
        "rsa" => {
            let bits = options
                .get("modulusLength")
                .and_then(Value::as_u64)
                .unwrap_or(2048) as u32;
            let exponent = options
                .get("publicExponent")
                .map(|value| javascript_crypto_u32_from_bridge_value(value, "rsa publicExponent"))
                .transpose()?
                .unwrap_or(65_537);
            let exponent = BigNum::from_u32(exponent).map_err(javascript_crypto_openssl_error)?;
            let rsa =
                Rsa::generate_with_e(bits, &exponent).map_err(javascript_crypto_openssl_error)?;
            PKey::from_rsa(rsa).map_err(javascript_crypto_openssl_error)?
        }
        "ec" => {
            let named_curve = options
                .get("namedCurve")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    SidecarError::InvalidState(String::from(
                        "crypto.generateKeyPairSync ec requires namedCurve",
                    ))
                })?;
            let group = EcGroup::from_curve_name(javascript_crypto_curve_nid(named_curve)?)
                .map_err(javascript_crypto_openssl_error)?;
            let key = EcKey::generate(&group).map_err(javascript_crypto_openssl_error)?;
            PKey::from_ec_key(key).map_err(javascript_crypto_openssl_error)?
        }
        "ed25519" => PKey::generate_ed25519().map_err(javascript_crypto_openssl_error)?,
        "x25519" => PKey::generate_x25519().map_err(javascript_crypto_openssl_error)?,
        other => {
            return Err(SidecarError::InvalidState(format!(
                "unsupported crypto key pair type {other}"
            )));
        }
    };
    let public_key = PKey::public_key_from_pem(
        &private_key
            .public_key_to_pem()
            .map_err(javascript_crypto_openssl_error)?,
    )
    .map_err(javascript_crypto_openssl_error)?;
    let response = if public_encoding.is_some() || private_encoding.is_some() {
        json!({
            "publicKey": javascript_crypto_serialize_encoded_key_value_public(&public_key, public_encoding.as_ref())?,
            "privateKey": javascript_crypto_serialize_encoded_key_value_private(&private_key, private_encoding.as_ref())?,
        })
    } else {
        json!({
            "publicKey": javascript_crypto_serialize_sandbox_key_object(&JavascriptCryptoKeyMaterial::Public(public_key))?,
            "privateKey": javascript_crypto_serialize_sandbox_key_object(&JavascriptCryptoKeyMaterial::Private(private_key))?,
        })
    };
    Ok(Value::String(serde_json::to_string(&response).map_err(
        |error| SidecarError::InvalidState(format!("serialize generated key pair: {error}")),
    )?))
}

fn service_javascript_crypto_generate_key_sync_rpc(
    request: &HostRpcRequest,
) -> Result<Value, SidecarError> {
    let key_type = javascript_sync_rpc_arg_str(&request.args, 0, "crypto.generateKeySync type")?;
    let options = javascript_crypto_parse_serialized_options_arg(
        &request.args,
        1,
        "crypto.generateKeySync options",
    )?
    .unwrap_or(Value::Object(Map::new()));
    let bit_length = options
        .get("length")
        .and_then(Value::as_u64)
        .ok_or_else(|| {
            SidecarError::InvalidState(String::from(
                "crypto.generateKeySync options.length is required",
            ))
        })? as usize;
    let mut raw = vec![0_u8; bit_length.div_ceil(8)];
    rand_bytes(&mut raw).map_err(javascript_crypto_openssl_error)?;
    let serialized = match key_type {
        "hmac" => javascript_crypto_serialize_sandbox_key_object(
            &JavascriptCryptoKeyMaterial::Secret(raw),
        )?,
        "aes" => javascript_crypto_serialize_sandbox_key_object(
            &JavascriptCryptoKeyMaterial::Secret(raw),
        )?,
        other => {
            return Err(SidecarError::InvalidState(format!(
                "unsupported crypto.generateKeySync type {other}"
            )));
        }
    };
    Ok(Value::String(serde_json::to_string(&serialized).map_err(
        |error| SidecarError::InvalidState(format!("serialize generated key: {error}")),
    )?))
}

fn service_javascript_crypto_generate_prime_sync_rpc(
    request: &HostRpcRequest,
) -> Result<Value, SidecarError> {
    let bits =
        javascript_sync_rpc_arg_u64(&request.args, 0, "crypto.generatePrimeSync size")? as i32;
    let options = javascript_crypto_parse_serialized_options_arg(
        &request.args,
        1,
        "crypto.generatePrimeSync options",
    )?
    .unwrap_or(Value::Object(Map::new()));
    let safe = options
        .get("safe")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let add = options
        .get("add")
        .map(|value| javascript_crypto_bignum_from_bridge_value(value, "prime add"))
        .transpose()?;
    let rem = options
        .get("rem")
        .map(|value| javascript_crypto_bignum_from_bridge_value(value, "prime rem"))
        .transpose()?;
    let mut prime = BigNum::new().map_err(javascript_crypto_openssl_error)?;
    prime
        .generate_prime(bits, safe, add.as_deref(), rem.as_deref())
        .map_err(javascript_crypto_openssl_error)?;
    let payload = if options
        .get("bigint")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        json!({
            "__type": "bigint",
            "value": prime.to_dec_str().map_err(javascript_crypto_openssl_error)?.to_string(),
        })
    } else {
        json!({
            "__type": "buffer",
            "value": base64::engine::general_purpose::STANDARD.encode(prime.to_vec()),
        })
    };
    Ok(Value::String(serde_json::to_string(&payload).map_err(
        |error| SidecarError::InvalidState(format!("serialize generated prime: {error}")),
    )?))
}

fn service_javascript_crypto_diffie_hellman_sync_rpc(
    request: &HostRpcRequest,
) -> Result<Value, SidecarError> {
    let options = javascript_sync_rpc_arg_str(&request.args, 0, "crypto.diffieHellman options")?;
    let parsed: Value = serde_json::from_str(options).map_err(|error| {
        SidecarError::InvalidState(format!(
            "crypto.diffieHellman options must be valid JSON: {error}"
        ))
    })?;
    let private_key = javascript_crypto_parse_key_material_value(
        parsed.get("privateKey").ok_or_else(|| {
            SidecarError::InvalidState(String::from("crypto.diffieHellman missing privateKey"))
        })?,
        Some("private"),
        "crypto.diffieHellman privateKey",
    )?;
    let public_key = javascript_crypto_parse_key_material_value(
        parsed.get("publicKey").ok_or_else(|| {
            SidecarError::InvalidState(String::from("crypto.diffieHellman missing publicKey"))
        })?,
        Some("public"),
        "crypto.diffieHellman publicKey",
    )?;
    let private_key =
        javascript_crypto_expect_private_key(private_key, "crypto.diffieHellman privateKey")?;
    let public_key =
        javascript_crypto_expect_public_key(public_key, "crypto.diffieHellman publicKey")?;
    let mut deriver = Deriver::new(&private_key).map_err(javascript_crypto_openssl_error)?;
    deriver
        .set_peer(&public_key)
        .map_err(javascript_crypto_openssl_error)?;
    let secret = deriver
        .derive_to_vec()
        .map_err(javascript_crypto_openssl_error)?;
    Ok(Value::String(
        serde_json::to_string(&json!({
            "__type": "buffer",
            "value": base64::engine::general_purpose::STANDARD.encode(secret),
        }))
        .map_err(|error| {
            SidecarError::InvalidState(format!("serialize derived secret: {error}"))
        })?,
    ))
}

fn service_javascript_crypto_diffie_hellman_group_sync_rpc(
    request: &HostRpcRequest,
) -> Result<Value, SidecarError> {
    let name = javascript_sync_rpc_arg_str(&request.args, 0, "crypto.diffieHellmanGroup name")?;
    let params = javascript_crypto_named_dh_group(name)?;
    let response = json!({
        "prime": {
            "__type": "buffer",
            "value": base64::engine::general_purpose::STANDARD.encode(params.prime_p().to_vec()),
        },
        "generator": {
            "__type": "buffer",
            "value": base64::engine::general_purpose::STANDARD.encode(params.generator().to_vec()),
        },
    });
    Ok(Value::String(serde_json::to_string(&response).map_err(
        |error| {
            SidecarError::InvalidState(format!("serialize diffieHellmanGroup response: {error}"))
        },
    )?))
}

fn service_javascript_crypto_diffie_hellman_session_create_sync_rpc(
    process: &mut ActiveProcess,
    request: &HostRpcRequest,
) -> Result<Value, SidecarError> {
    ensure_per_process_state_handle_capacity(
        process.diffie_hellman_sessions.len(),
        "diffie-hellman session",
    )?;
    let raw = javascript_sync_rpc_arg_str(
        &request.args,
        0,
        "crypto.diffieHellmanSessionCreate request",
    )?;
    let parsed: Value = serde_json::from_str(raw).map_err(|error| {
        SidecarError::InvalidState(format!(
            "crypto.diffieHellmanSessionCreate request must be valid JSON: {error}"
        ))
    })?;
    let session = match parsed.get("type").and_then(Value::as_str) {
        Some("group") => {
            let name = parsed.get("name").and_then(Value::as_str).ok_or_else(|| {
                SidecarError::InvalidState(String::from(
                    "crypto.diffieHellmanSessionCreate group requires name",
                ))
            })?;
            ActiveDiffieHellmanSession::Dh(ActiveDhSession {
                params: javascript_crypto_named_dh_group(name)?,
                key_pair: None,
            })
        }
        Some("dh") => {
            let args = parsed
                .get("args")
                .and_then(Value::as_array)
                .ok_or_else(|| {
                    SidecarError::InvalidState(String::from(
                        "crypto.diffieHellmanSessionCreate dh requires args",
                    ))
                })?;
            let params = javascript_crypto_build_dh_params(args)?;
            ActiveDiffieHellmanSession::Dh(ActiveDhSession {
                params,
                key_pair: None,
            })
        }
        Some("ecdh") => {
            let curve = parsed.get("name").and_then(Value::as_str).ok_or_else(|| {
                SidecarError::InvalidState(String::from(
                    "crypto.diffieHellmanSessionCreate ecdh requires name",
                ))
            })?;
            ActiveDiffieHellmanSession::Ecdh(ActiveEcdhSession {
                curve: curve.to_string(),
                key_pair: None,
            })
        }
        other => {
            return Err(SidecarError::InvalidState(format!(
                "Unsupported Diffie-Hellman session type: {}",
                other.unwrap_or("<missing>")
            )));
        }
    };
    process.next_diffie_hellman_session_id += 1;
    let session_id = process.next_diffie_hellman_session_id;
    process.diffie_hellman_sessions.insert(session_id, session);
    Ok(json!(session_id))
}

fn service_javascript_crypto_diffie_hellman_session_call_sync_rpc(
    process: &mut ActiveProcess,
    request: &HostRpcRequest,
) -> Result<Value, SidecarError> {
    let session_id = javascript_sync_rpc_arg_u64(
        &request.args,
        0,
        "crypto.diffieHellmanSessionCall session id",
    )?;
    let raw =
        javascript_sync_rpc_arg_str(&request.args, 1, "crypto.diffieHellmanSessionCall request")?;
    let parsed: Value = serde_json::from_str(raw).map_err(|error| {
        SidecarError::InvalidState(format!(
            "crypto.diffieHellmanSessionCall request must be valid JSON: {error}"
        ))
    })?;
    let method = parsed
        .get("method")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            SidecarError::InvalidState(String::from(
                "crypto.diffieHellmanSessionCall request missing method",
            ))
        })?;
    let args = parsed
        .get("args")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let session = process
        .diffie_hellman_sessions
        .get_mut(&session_id)
        .ok_or_else(|| {
            SidecarError::InvalidState(format!("Diffie-Hellman session {session_id} not found"))
        })?;
    let (result, has_result) = match session {
        ActiveDiffieHellmanSession::Dh(session) => {
            javascript_crypto_call_dh_session(session, method, &args)?
        }
        ActiveDiffieHellmanSession::Ecdh(session) => {
            javascript_crypto_call_ecdh_session(session, method, &args)?
        }
    };
    Ok(Value::String(
        serde_json::to_string(&json!({
            "result": result,
            "hasResult": has_result,
        }))
        .map_err(|error| {
            SidecarError::InvalidState(format!("serialize diffie session result: {error}"))
        })?,
    ))
}

fn service_javascript_crypto_diffie_hellman_session_destroy_sync_rpc(
    process: &mut ActiveProcess,
    request: &HostRpcRequest,
) -> Result<Value, SidecarError> {
    let session_id = javascript_sync_rpc_arg_u64(
        &request.args,
        0,
        "crypto.diffieHellmanSessionDestroy session id",
    )?;
    process
        .diffie_hellman_sessions
        .remove(&session_id)
        .ok_or_else(|| {
            SidecarError::InvalidState(format!("Diffie-Hellman session {session_id} not found"))
        })?;
    Ok(Value::Null)
}

fn service_javascript_crypto_subtle_sync_rpc(
    request: &HostRpcRequest,
) -> Result<Value, SidecarError> {
    let raw = javascript_sync_rpc_arg_str(&request.args, 0, "crypto.subtle request")?;
    let parsed: Value = serde_json::from_str(raw).map_err(|error| {
        SidecarError::InvalidState(format!("crypto.subtle request must be valid JSON: {error}"))
    })?;
    let op = parsed.get("op").and_then(Value::as_str).ok_or_else(|| {
        SidecarError::InvalidState(String::from("crypto.subtle request missing op"))
    })?;
    match op {
        "digest" => {
            let algorithm = parsed
                .get("algorithm")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    SidecarError::InvalidState(String::from(
                        "crypto.subtle.digest missing algorithm",
                    ))
                })?;
            let data = parsed.get("data").and_then(Value::as_str).ok_or_else(|| {
                SidecarError::InvalidState(String::from("crypto.subtle.digest missing data"))
            })?;
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(data)
                .map_err(|error| {
                    SidecarError::InvalidState(format!("crypto.subtle.digest data base64: {error}"))
                })?;
            let digest = JavascriptCryptoDigestAlgorithm::parse(algorithm)?.digest(&bytes);
            Ok(Value::String(
                serde_json::to_string(&json!({
                    "data": base64::engine::general_purpose::STANDARD.encode(digest),
                }))
                .map_err(|error| {
                    SidecarError::InvalidState(format!("serialize crypto.subtle digest: {error}"))
                })?,
            ))
        }
        "generateKey" => {
            let algorithm = parsed.get("algorithm").ok_or_else(|| {
                SidecarError::InvalidState(String::from(
                    "crypto.subtle.generateKey missing algorithm",
                ))
            })?;
            let name =
                javascript_crypto_subtle_algorithm_name(algorithm, "crypto.subtle.generateKey")?;
            if !matches!(name, "AES-GCM" | "AES-CBC" | "AES-CTR" | "AES-KW") {
                return Err(SidecarError::InvalidState(format!(
                    "Unsupported key algorithm: {name}"
                )));
            }
            let length_bits = algorithm
                .get("length")
                .and_then(Value::as_u64)
                .ok_or_else(|| {
                    SidecarError::InvalidState(String::from(
                        "crypto.subtle.generateKey AES algorithm requires length",
                    ))
                })?;
            if length_bits % 8 != 0 {
                return Err(SidecarError::InvalidState(String::from(
                    "crypto.subtle.generateKey length must be byte-aligned",
                )));
            }
            let length_bytes = usize::try_from(length_bits / 8).map_err(|_| {
                SidecarError::InvalidState(String::from(
                    "crypto.subtle.generateKey length is too large",
                ))
            })?;
            let mut raw = vec![0_u8; length_bytes];
            rand_bytes(&mut raw).map_err(javascript_crypto_openssl_error)?;
            let key = javascript_crypto_serialize_subtle_secret_key(
                &raw,
                javascript_crypto_normalize_subtle_secret_algorithm(algorithm.clone(), &raw)?,
                parsed
                    .get("extractable")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                parsed.get("usages").cloned().unwrap_or_else(|| json!([])),
            )?;
            Ok(Value::String(
                serde_json::to_string(&json!({ "key": key })).map_err(|error| {
                    SidecarError::InvalidState(format!(
                        "serialize crypto.subtle generated key: {error}"
                    ))
                })?,
            ))
        }
        "importKey" => {
            let format = parsed
                .get("format")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    SidecarError::InvalidState(String::from(
                        "crypto.subtle.importKey missing format",
                    ))
                })?;
            if format != "raw" {
                return Err(SidecarError::InvalidState(format!(
                    "Unsupported import format: {format}"
                )));
            }
            let key_data = parsed
                .get("keyData")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    SidecarError::InvalidState(String::from(
                        "crypto.subtle.importKey missing keyData",
                    ))
                })?;
            let raw = base64::engine::general_purpose::STANDARD
                .decode(key_data)
                .map_err(|error| {
                    SidecarError::InvalidState(format!(
                        "crypto.subtle.importKey keyData base64: {error}"
                    ))
                })?;
            let algorithm = parsed.get("algorithm").ok_or_else(|| {
                SidecarError::InvalidState(String::from(
                    "crypto.subtle.importKey missing algorithm",
                ))
            })?;
            let key = javascript_crypto_serialize_subtle_secret_key(
                &raw,
                javascript_crypto_normalize_subtle_secret_algorithm(algorithm.clone(), &raw)?,
                parsed
                    .get("extractable")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                parsed.get("usages").cloned().unwrap_or_else(|| json!([])),
            )?;
            Ok(Value::String(
                serde_json::to_string(&json!({ "key": key })).map_err(|error| {
                    SidecarError::InvalidState(format!(
                        "serialize crypto.subtle imported key: {error}"
                    ))
                })?,
            ))
        }
        "exportKey" => {
            let format = parsed
                .get("format")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    SidecarError::InvalidState(String::from(
                        "crypto.subtle.exportKey missing format",
                    ))
                })?;
            if format != "raw" {
                return Err(SidecarError::InvalidState(format!(
                    "Unsupported export format: {format}"
                )));
            }
            let raw = javascript_crypto_subtle_key_raw(
                parsed.get("key").ok_or_else(|| {
                    SidecarError::InvalidState(String::from("crypto.subtle.exportKey missing key"))
                })?,
                "crypto.subtle.exportKey key",
            )?;
            Ok(Value::String(
                serde_json::to_string(&json!({
                    "data": base64::engine::general_purpose::STANDARD.encode(raw),
                }))
                .map_err(|error| {
                    SidecarError::InvalidState(format!("serialize crypto.subtle export: {error}"))
                })?,
            ))
        }
        "encrypt" | "decrypt" => service_javascript_crypto_subtle_aes_crypt_sync_rpc(op, &parsed),
        "sign" | "verify" => service_javascript_crypto_subtle_hmac_sync_rpc(op, &parsed),
        _ => Err(SidecarError::InvalidState(format!(
            "Unsupported subtle operation: {op}"
        ))),
    }
}

fn service_javascript_crypto_subtle_hmac_sync_rpc(
    op: &str,
    parsed: &Value,
) -> Result<Value, SidecarError> {
    let algorithm = parsed.get("algorithm").ok_or_else(|| {
        SidecarError::InvalidState(format!("crypto.subtle.{op} missing algorithm"))
    })?;
    let name = javascript_crypto_subtle_algorithm_name(algorithm, &format!("crypto.subtle.{op}"))?;
    if name != "HMAC" {
        return Err(SidecarError::InvalidState(format!(
            "Unsupported subtle {op} algorithm: {name}"
        )));
    }
    let hash = algorithm.get("hash").ok_or_else(|| {
        SidecarError::InvalidState(format!("crypto.subtle.{op} HMAC algorithm missing hash"))
    })?;
    let hash_name =
        javascript_crypto_subtle_algorithm_name(hash, &format!("crypto.subtle.{op} HMAC hash"))?;
    let digest = JavascriptCryptoDigestAlgorithm::parse(hash_name)?;
    let key = javascript_crypto_subtle_key_raw(
        parsed
            .get("key")
            .ok_or_else(|| SidecarError::InvalidState(format!("crypto.subtle.{op} missing key")))?,
        &format!("crypto.subtle.{op} key"),
    )?;
    let data = javascript_crypto_subtle_base64_field(parsed, "data", op)?;
    let mac = digest.hmac(&key, &data)?;
    if op == "verify" {
        let signature = javascript_crypto_subtle_base64_field(parsed, "signature", op)?;
        return Ok(Value::String(
            serde_json::to_string(&json!({
                "result": openssl::memcmp::eq(&mac, &signature),
            }))
            .map_err(|error| {
                SidecarError::InvalidState(format!("serialize crypto.subtle verify: {error}"))
            })?,
        ));
    }
    Ok(Value::String(
        serde_json::to_string(&json!({
            "data": base64::engine::general_purpose::STANDARD.encode(mac),
        }))
        .map_err(|error| {
            SidecarError::InvalidState(format!("serialize crypto.subtle sign: {error}"))
        })?,
    ))
}

fn javascript_crypto_subtle_base64_field(
    parsed: &Value,
    field: &str,
    op: &str,
) -> Result<Vec<u8>, SidecarError> {
    let value = parsed
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| SidecarError::InvalidState(format!("crypto.subtle.{op} missing {field}")))?;
    base64::engine::general_purpose::STANDARD
        .decode(value)
        .map_err(|error| {
            SidecarError::InvalidState(format!("crypto.subtle.{op} {field} base64: {error}"))
        })
}

fn javascript_crypto_subtle_algorithm_name<'a>(
    algorithm: &'a Value,
    label: &str,
) -> Result<&'a str, SidecarError> {
    if let Some(name) = algorithm.as_str() {
        return Ok(name);
    }
    algorithm
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| SidecarError::InvalidState(format!("{label} algorithm missing name")))
}

fn javascript_crypto_normalize_subtle_secret_algorithm(
    algorithm: Value,
    raw: &[u8],
) -> Result<Value, SidecarError> {
    let mut object = match algorithm {
        Value::String(name) => {
            let mut object = Map::new();
            object.insert(String::from("name"), Value::String(name));
            object
        }
        Value::Object(object) => object,
        _ => {
            return Err(SidecarError::InvalidState(String::from(
                "crypto.subtle secret algorithm must be a string or object",
            )));
        }
    };
    let name = object
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            SidecarError::InvalidState(String::from("crypto.subtle secret algorithm missing name"))
        })?
        .to_string();
    if matches!(name.as_str(), "AES-GCM" | "AES-CBC" | "AES-CTR" | "AES-KW")
        && !object.contains_key("length")
    {
        object.insert(String::from("length"), json!(raw.len() * 8));
    }
    Ok(Value::Object(object))
}

fn javascript_crypto_serialize_subtle_secret_key(
    raw: &[u8],
    algorithm: Value,
    extractable: bool,
    usages: Value,
) -> Result<Value, SidecarError> {
    let raw_base64 = base64::engine::general_purpose::STANDARD.encode(raw);
    let source_key_object_data = javascript_crypto_serialize_sandbox_key_object(
        &JavascriptCryptoKeyMaterial::Secret(raw.to_vec()),
    )?;
    Ok(json!({
        "type": "secret",
        "algorithm": algorithm,
        "extractable": extractable,
        "usages": usages,
        "_raw": raw_base64,
        "_sourceKeyObjectData": source_key_object_data,
    }))
}

fn javascript_crypto_subtle_key_raw(key: &Value, label: &str) -> Result<Vec<u8>, SidecarError> {
    let raw = key.get("_raw").and_then(Value::as_str).ok_or_else(|| {
        SidecarError::InvalidState(format!("{label} must be a raw secret CryptoKey"))
    })?;
    base64::engine::general_purpose::STANDARD
        .decode(raw)
        .map_err(|error| SidecarError::InvalidState(format!("{label} raw base64: {error}")))
}

fn service_javascript_crypto_subtle_aes_crypt_sync_rpc(
    op: &str,
    parsed: &Value,
) -> Result<Value, SidecarError> {
    let algorithm = parsed.get("algorithm").ok_or_else(|| {
        SidecarError::InvalidState(format!("crypto.subtle.{op} missing algorithm"))
    })?;
    let name = javascript_crypto_subtle_algorithm_name(algorithm, &format!("crypto.subtle.{op}"))?;
    if !matches!(name, "AES-GCM" | "AES-CBC") {
        return Err(SidecarError::InvalidState(format!(
            "Unsupported subtle AES operation algorithm: {name}"
        )));
    }
    let key = javascript_crypto_subtle_key_raw(
        parsed
            .get("key")
            .ok_or_else(|| SidecarError::InvalidState(format!("crypto.subtle.{op} missing key")))?,
        &format!("crypto.subtle.{op} key"),
    )?;
    let iv = algorithm.get("iv").and_then(Value::as_str).ok_or_else(|| {
        SidecarError::InvalidState(format!("crypto.subtle.{op} {name} missing iv"))
    })?;
    let iv = base64::engine::general_purpose::STANDARD
        .decode(iv)
        .map_err(|error| {
            SidecarError::InvalidState(format!("crypto.subtle.{op} iv base64: {error}"))
        })?;
    let data = parsed
        .get("data")
        .and_then(Value::as_str)
        .ok_or_else(|| SidecarError::InvalidState(format!("crypto.subtle.{op} missing data")))?;
    let mut data = base64::engine::general_purpose::STANDARD
        .decode(data)
        .map_err(|error| {
            SidecarError::InvalidState(format!("crypto.subtle.{op} data base64: {error}"))
        })?;
    if name == "AES-CBC" {
        if iv.len() != 16 {
            return Err(SidecarError::InvalidState(format!(
                "crypto.subtle.{op} AES-CBC iv must be 16 bytes"
            )));
        }
        let cipher_name = format!("aes-{}-cbc", key.len() * 8);
        let mut session = javascript_crypto_build_cipher_session(
            &cipher_name,
            &key,
            Some(&iv),
            op == "decrypt",
            None,
        )?;
        let mut output = javascript_crypto_cipher_update(&mut session, &data)?;
        output.extend(
            session
                .finalize()
                .map_err(javascript_crypto_cipher_error)?
                .data,
        );
        return Ok(Value::String(
            serde_json::to_string(&json!({
                "data": base64::engine::general_purpose::STANDARD.encode(output),
            }))
            .map_err(|error| {
                SidecarError::InvalidState(format!("serialize crypto.subtle {op}: {error}"))
            })?,
        ));
    }
    let tag_len = javascript_crypto_subtle_aes_gcm_tag_len(algorithm)?;
    let mut options = Map::new();
    options.insert(String::from("authTagLength"), json!(tag_len));
    if let Some(additional_data) = algorithm.get("additionalData").and_then(Value::as_str) {
        options.insert(
            String::from("aad"),
            Value::String(additional_data.to_string()),
        );
    }
    let decrypt = op == "decrypt";
    if decrypt {
        if data.len() < tag_len {
            return Err(SidecarError::InvalidState(String::from(
                "crypto.subtle.decrypt AES-GCM data shorter than auth tag",
            )));
        }
        let auth_tag = data.split_off(data.len() - tag_len);
        options.insert(
            String::from("authTag"),
            Value::String(base64::engine::general_purpose::STANDARD.encode(auth_tag)),
        );
    }
    let cipher_name = format!("aes-{}-gcm", key.len() * 8);
    let mut session = javascript_crypto_build_cipher_session(
        &cipher_name,
        &key,
        Some(&iv),
        decrypt,
        Some(&Value::Object(options)),
    )?;
    let mut output = javascript_crypto_cipher_update(&mut session, &data)?;
    let outcome = session.finalize().map_err(javascript_crypto_cipher_error)?;
    output.extend(outcome.data);
    if !decrypt {
        if let Some(auth_tag) = outcome.auth_tag {
            output.extend(auth_tag);
        }
    }
    Ok(Value::String(
        serde_json::to_string(&json!({
            "data": base64::engine::general_purpose::STANDARD.encode(output),
        }))
        .map_err(|error| {
            SidecarError::InvalidState(format!("serialize crypto.subtle {op}: {error}"))
        })?,
    ))
}

fn javascript_crypto_subtle_aes_gcm_tag_len(algorithm: &Value) -> Result<usize, SidecarError> {
    let tag_bits = algorithm
        .get("tagLength")
        .and_then(Value::as_u64)
        .unwrap_or(128);
    if !tag_bits.is_multiple_of(8) {
        return Err(SidecarError::InvalidState(String::from(
            "crypto.subtle AES-GCM tagLength must be byte-aligned",
        )));
    }
    usize::try_from(tag_bits / 8).map_err(|_| {
        SidecarError::InvalidState(String::from("crypto.subtle AES-GCM tagLength too large"))
    })
}

fn service_javascript_crypto_cipheriv_inner(
    request: &HostRpcRequest,
    decrypt: bool,
) -> Result<Value, SidecarError> {
    let label = if decrypt {
        "crypto.decipheriv"
    } else {
        "crypto.cipheriv"
    };
    let algorithm = javascript_sync_rpc_arg_str(&request.args, 0, &format!("{label} algorithm"))?;
    let key = javascript_sync_rpc_base64_arg(&request.args, 1, &format!("{label} key"))?;
    let iv = javascript_sync_rpc_base64_arg_optional(&request.args, 2, &format!("{label} iv"))?;
    let data = javascript_sync_rpc_base64_arg(&request.args, 3, &format!("{label} data"))?;
    let options =
        javascript_sync_rpc_json_arg_optional(&request.args, 4, &format!("{label} options"))?;
    let mut session = javascript_crypto_build_cipher_session(
        algorithm,
        &key,
        iv.as_deref(),
        decrypt,
        options.as_ref(),
    )?;
    let payload = javascript_crypto_cipher_update(&mut session, &data)?;
    let outcome = session.finalize().map_err(javascript_crypto_cipher_error)?;
    if decrypt {
        let mut output = payload;
        output.extend(outcome.data);
        return Ok(Value::String(
            base64::engine::general_purpose::STANDARD.encode(output),
        ));
    }

    let mut response = Map::new();
    let mut encrypted = payload;
    encrypted.extend(outcome.data);
    response.insert(
        String::from("data"),
        Value::String(base64::engine::general_purpose::STANDARD.encode(encrypted)),
    );
    if let Some(auth_tag) = outcome.auth_tag {
        response.insert(
            String::from("authTag"),
            Value::String(base64::engine::general_purpose::STANDARD.encode(auth_tag)),
        );
    }
    Ok(Value::String(serde_json::to_string(&response).map_err(
        |error| SidecarError::InvalidState(format!("serialize {label} response: {error}")),
    )?))
}

fn javascript_sync_rpc_base64_arg_optional(
    args: &[Value],
    index: usize,
    label: &str,
) -> Result<Option<Vec<u8>>, SidecarError> {
    if args.get(index).is_none() || args[index].is_null() {
        return Ok(None);
    }
    javascript_sync_rpc_base64_arg(args, index, label).map(Some)
}

fn javascript_sync_rpc_json_arg_optional(
    args: &[Value],
    index: usize,
    label: &str,
) -> Result<Option<Value>, SidecarError> {
    if args.get(index).is_none() || args[index].is_null() {
        return Ok(None);
    }
    let raw = javascript_sync_rpc_arg_str(args, index, label)?;
    serde_json::from_str(raw)
        .map(Some)
        .map_err(|error| SidecarError::InvalidState(format!("{label} must be valid JSON: {error}")))
}

fn javascript_crypto_parse_direct_key_input(
    raw: &str,
    expected: Option<&str>,
    label: &str,
) -> Result<JavascriptDirectKeyInput, SidecarError> {
    let parsed: Value = serde_json::from_str(raw).map_err(|error| {
        SidecarError::InvalidState(format!("{label} must be valid JSON: {error}"))
    })?;
    let padding = match parsed.as_object().and_then(|value| value.get("padding")) {
        Some(value) => javascript_crypto_padding_from_value(value)?,
        None => None,
    };
    Ok(JavascriptDirectKeyInput {
        key: javascript_crypto_parse_key_material_value(&parsed, expected, label)?,
        padding,
    })
}

fn javascript_crypto_parse_key_material_value(
    value: &Value,
    expected: Option<&str>,
    label: &str,
) -> Result<JavascriptCryptoKeyMaterial, SidecarError> {
    if let Some(object) = value.as_object() {
        if object.get("__type").and_then(Value::as_str) == Some("keyObject") {
            let serialized = object.get("value").ok_or_else(|| {
                SidecarError::InvalidState(format!("{label} keyObject is missing a value"))
            })?;
            return javascript_crypto_parse_serialized_key_object(serialized, expected, label);
        }
        if object.contains_key("type") && (object.contains_key("pem") || object.contains_key("raw"))
        {
            return javascript_crypto_parse_serialized_key_object(value, expected, label);
        }
        if let Some(source) = object.get("key") {
            return javascript_crypto_parse_key_source(
                source,
                object.get("format").and_then(Value::as_str),
                object.get("type").and_then(Value::as_str),
                expected,
                label,
            );
        }
    }
    javascript_crypto_parse_key_source(value, None, None, expected, label)
}

fn javascript_crypto_parse_key_source(
    source: &Value,
    format: Option<&str>,
    kind: Option<&str>,
    expected: Option<&str>,
    label: &str,
) -> Result<JavascriptCryptoKeyMaterial, SidecarError> {
    match source {
        Value::String(pem) => javascript_crypto_parse_key_from_pem(pem.as_bytes(), expected, label),
        Value::Object(object) if object.get("__type").and_then(Value::as_str) == Some("buffer") => {
            let data = javascript_crypto_decode_bridge_buffer(source, label)?;
            javascript_crypto_parse_key_from_bytes(&data, format, kind, expected, label)
        }
        Value::Object(_) => {
            if format == Some("jwk") {
                return Err(SidecarError::InvalidState(format!(
                    "{label} jwk inputs are not supported yet"
                )));
            }
            Err(SidecarError::InvalidState(format!(
                "{label} has an unsupported key shape"
            )))
        }
        _ => Err(SidecarError::InvalidState(format!(
            "{label} has an unsupported key value"
        ))),
    }
}

fn javascript_crypto_parse_key_from_pem(
    pem: &[u8],
    expected: Option<&str>,
    label: &str,
) -> Result<JavascriptCryptoKeyMaterial, SidecarError> {
    match expected {
        Some("private") => PKey::private_key_from_pem(pem)
            .map(JavascriptCryptoKeyMaterial::Private)
            .map_err(|error| {
                SidecarError::InvalidState(format!("{label} private key is invalid: {error}"))
            }),
        Some("public") => PKey::public_key_from_pem(pem)
            .map(JavascriptCryptoKeyMaterial::Public)
            .map_err(|error| {
                SidecarError::InvalidState(format!("{label} public key is invalid: {error}"))
            }),
        _ => PKey::private_key_from_pem(pem)
            .map(JavascriptCryptoKeyMaterial::Private)
            .or_else(|_| PKey::public_key_from_pem(pem).map(JavascriptCryptoKeyMaterial::Public))
            .map_err(|error| {
                SidecarError::InvalidState(format!("{label} PEM key is invalid: {error}"))
            }),
    }
}

fn javascript_crypto_parse_key_from_bytes(
    der: &[u8],
    format: Option<&str>,
    kind: Option<&str>,
    expected: Option<&str>,
    label: &str,
) -> Result<JavascriptCryptoKeyMaterial, SidecarError> {
    match (format.unwrap_or("der"), kind.or(expected)) {
        ("der", Some("pkcs8")) | ("der", Some("private")) => PKey::private_key_from_der(der)
            .map(JavascriptCryptoKeyMaterial::Private)
            .map_err(|error| {
                SidecarError::InvalidState(format!("{label} private key DER is invalid: {error}"))
            }),
        ("der", Some("spki")) | ("der", Some("public")) => PKey::public_key_from_der(der)
            .map(JavascriptCryptoKeyMaterial::Public)
            .map_err(|error| {
                SidecarError::InvalidState(format!("{label} public key DER is invalid: {error}"))
            }),
        _ => Err(SidecarError::InvalidState(format!(
            "{label} unsupported key bytes format"
        ))),
    }
}

fn javascript_crypto_parse_serialized_key_object(
    value: &Value,
    expected: Option<&str>,
    label: &str,
) -> Result<JavascriptCryptoKeyMaterial, SidecarError> {
    let serialized: JavascriptSerializedSandboxKeyObject = serde_json::from_value(value.clone())
        .map_err(|error| {
            SidecarError::InvalidState(format!("{label} keyObject is invalid: {error}"))
        })?;
    match serialized.kind.as_str() {
        "secret" => {
            if expected == Some("public") || expected == Some("private") {
                return Err(SidecarError::InvalidState(format!(
                    "{label} expected an asymmetric key"
                )));
            }
            Ok(JavascriptCryptoKeyMaterial::Secret(
                base64::engine::general_purpose::STANDARD
                    .decode(serialized.raw.unwrap_or_default())
                    .map_err(|error| {
                        SidecarError::InvalidState(format!(
                            "{label} secret key contains invalid base64: {error}"
                        ))
                    })?,
            ))
        }
        "private" => {
            let pem = serialized.pem.ok_or_else(|| {
                SidecarError::InvalidState(format!("{label} private keyObject is missing pem"))
            })?;
            javascript_crypto_parse_key_from_pem(pem.as_bytes(), Some("private"), label)
        }
        "public" => {
            let pem = serialized.pem.ok_or_else(|| {
                SidecarError::InvalidState(format!("{label} public keyObject is missing pem"))
            })?;
            javascript_crypto_parse_key_from_pem(pem.as_bytes(), Some("public"), label)
        }
        other => Err(SidecarError::InvalidState(format!(
            "{label} has unsupported keyObject type {other}"
        ))),
    }
}

fn javascript_crypto_expect_private_key(
    key: JavascriptCryptoKeyMaterial,
    label: &str,
) -> Result<PKey<Private>, SidecarError> {
    match key {
        JavascriptCryptoKeyMaterial::Private(key) => Ok(key),
        _ => Err(SidecarError::InvalidState(format!(
            "{label} requires a private key"
        ))),
    }
}

fn javascript_crypto_expect_public_key(
    key: JavascriptCryptoKeyMaterial,
    label: &str,
) -> Result<PKey<Public>, SidecarError> {
    match key {
        JavascriptCryptoKeyMaterial::Public(key) => Ok(key),
        JavascriptCryptoKeyMaterial::Private(key) => {
            let pem = key
                .public_key_to_pem()
                .map_err(javascript_crypto_openssl_error)?;
            PKey::public_key_from_pem(&pem).map_err(javascript_crypto_openssl_error)
        }
        _ => Err(SidecarError::InvalidState(format!(
            "{label} requires a public key"
        ))),
    }
}

fn javascript_crypto_new_signer<'a>(
    algorithm: Option<&'a str>,
    key: &'a PKey<Private>,
) -> Result<Signer<'a>, SidecarError> {
    if matches!(key.id(), PKeyId::ED25519 | PKeyId::ED448) || algorithm.is_none() {
        return Signer::new_without_digest(key).map_err(javascript_crypto_openssl_error);
    }
    Signer::new(
        javascript_crypto_message_digest_from_name(algorithm.ok_or_else(|| {
            SidecarError::InvalidState(String::from("crypto.sign requires a digest algorithm"))
        })?)?,
        key,
    )
    .map_err(javascript_crypto_openssl_error)
}

fn javascript_crypto_new_verifier<'a>(
    algorithm: Option<&'a str>,
    key: &'a PKey<Public>,
) -> Result<Verifier<'a>, SidecarError> {
    if matches!(key.id(), PKeyId::ED25519 | PKeyId::ED448) || algorithm.is_none() {
        return Verifier::new_without_digest(key).map_err(javascript_crypto_openssl_error);
    }
    Verifier::new(
        javascript_crypto_message_digest_from_name(algorithm.ok_or_else(|| {
            SidecarError::InvalidState(String::from("crypto.verify requires a digest algorithm"))
        })?)?,
        key,
    )
    .map_err(javascript_crypto_openssl_error)
}

fn javascript_crypto_message_digest_from_name(name: &str) -> Result<MessageDigest, SidecarError> {
    match name.trim().to_ascii_lowercase().replace('-', "").as_str() {
        "md5" => Ok(MessageDigest::md5()),
        "sha1" => Ok(MessageDigest::sha1()),
        "sha256" => Ok(MessageDigest::sha256()),
        "sha384" => Ok(MessageDigest::sha384()),
        "sha512" => Ok(MessageDigest::sha512()),
        other => Err(SidecarError::InvalidState(format!(
            "unsupported crypto digest algorithm {other}"
        ))),
    }
}

fn javascript_crypto_padding_from_value(value: &Value) -> Result<Option<Padding>, SidecarError> {
    let Some(number) = value.as_i64() else {
        return Ok(None);
    };
    let padding = match number {
        1 => Padding::PKCS1,
        3 => Padding::NONE,
        4 => Padding::PKCS1_OAEP,
        6 => Padding::PKCS1_PSS,
        other => {
            return Err(SidecarError::InvalidState(format!(
                "unsupported RSA padding constant {other}"
            )));
        }
    };
    Ok(Some(padding))
}

fn javascript_crypto_decode_bridge_buffer(
    value: &Value,
    label: &str,
) -> Result<Vec<u8>, SidecarError> {
    decode_bridge_buffer_value(value)
        .map_err(|error| SidecarError::InvalidState(format!("{label} {error}")))
}

fn javascript_crypto_serialize_sandbox_key_object(
    key: &JavascriptCryptoKeyMaterial,
) -> Result<Value, SidecarError> {
    let serialized = match key {
        JavascriptCryptoKeyMaterial::Private(key) => JavascriptSerializedSandboxKeyObject {
            kind: String::from("private"),
            pem: Some(
                String::from_utf8(
                    key.private_key_to_pem_pkcs8()
                        .map_err(javascript_crypto_openssl_error)?,
                )
                .map_err(|error| {
                    SidecarError::InvalidState(format!("private key PEM is not utf8: {error}"))
                })?,
            ),
            raw: None,
            asymmetric_key_type: javascript_crypto_pkey_type_name(key.id()),
            asymmetric_key_details: None,
            jwk: None,
        },
        JavascriptCryptoKeyMaterial::Public(key) => JavascriptSerializedSandboxKeyObject {
            kind: String::from("public"),
            pem: Some(
                String::from_utf8(
                    key.public_key_to_pem()
                        .map_err(javascript_crypto_openssl_error)?,
                )
                .map_err(|error| {
                    SidecarError::InvalidState(format!("public key PEM is not utf8: {error}"))
                })?,
            ),
            raw: None,
            asymmetric_key_type: javascript_crypto_pkey_type_name(key.id()),
            asymmetric_key_details: None,
            jwk: None,
        },
        JavascriptCryptoKeyMaterial::Secret(raw) => JavascriptSerializedSandboxKeyObject {
            kind: String::from("secret"),
            pem: None,
            raw: Some(base64::engine::general_purpose::STANDARD.encode(raw)),
            asymmetric_key_type: None,
            asymmetric_key_details: None,
            jwk: None,
        },
    };
    serde_json::to_value(serialized)
        .map_err(|error| SidecarError::InvalidState(format!("serialize key object: {error}")))
}

fn javascript_crypto_pkey_type_name(id: PKeyId) -> Option<String> {
    match id {
        PKeyId::RSA => Some(String::from("rsa")),
        PKeyId::EC => Some(String::from("ec")),
        PKeyId::ED25519 => Some(String::from("ed25519")),
        PKeyId::ED448 => Some(String::from("ed448")),
        PKeyId::X25519 => Some(String::from("x25519")),
        PKeyId::X448 => Some(String::from("x448")),
        PKeyId::DH => Some(String::from("dh")),
        _ => None,
    }
}

fn javascript_crypto_rsa_output_size(
    key: &JavascriptCryptoKeyMaterial,
) -> Result<usize, SidecarError> {
    match key {
        JavascriptCryptoKeyMaterial::Private(key) => key
            .rsa()
            .map(|rsa| rsa.size() as usize)
            .map_err(javascript_crypto_openssl_error),
        JavascriptCryptoKeyMaterial::Public(key) => key
            .rsa()
            .map(|rsa| rsa.size() as usize)
            .map_err(javascript_crypto_openssl_error),
        JavascriptCryptoKeyMaterial::Secret(_) => Err(SidecarError::InvalidState(String::from(
            "RSA operations require an asymmetric key",
        ))),
    }
}

fn javascript_crypto_parse_serialized_options_arg(
    args: &[Value],
    index: usize,
    label: &str,
) -> Result<Option<Value>, SidecarError> {
    let Some(raw) = args.get(index).and_then(Value::as_str) else {
        return Ok(None);
    };
    let parsed: Value = serde_json::from_str(raw).map_err(|error| {
        SidecarError::InvalidState(format!("{label} must be valid JSON: {error}"))
    })?;
    if parsed.get("hasOptions").and_then(Value::as_bool) == Some(true) {
        Ok(parsed.get("options").cloned())
    } else {
        Ok(None)
    }
}

fn javascript_crypto_u32_from_bridge_value(
    value: &Value,
    label: &str,
) -> Result<u32, SidecarError> {
    if let Some(number) = value.as_u64() {
        return u32::try_from(number)
            .map_err(|_| SidecarError::InvalidState(format!("{label} must fit within u32")));
    }
    let bytes = javascript_crypto_decode_bridge_buffer(value, label)?;
    if bytes.len() > 4 {
        return Err(SidecarError::InvalidState(format!(
            "{label} buffer is too large for u32"
        )));
    }
    Ok(bytes
        .into_iter()
        .fold(0_u32, |acc, byte| (acc << 8) | u32::from(byte)))
}

fn javascript_crypto_bignum_from_bridge_value(
    value: &Value,
    label: &str,
) -> Result<BigNum, SidecarError> {
    if let Some(object) = value.as_object() {
        if object.get("__type").and_then(Value::as_str) == Some("bigint") {
            let decimal = object.get("value").and_then(Value::as_str).ok_or_else(|| {
                SidecarError::InvalidState(format!("{label} bigint is missing a value"))
            })?;
            return BigNum::from_dec_str(decimal).map_err(javascript_crypto_openssl_error);
        }
    }
    let bytes = javascript_crypto_decode_bridge_buffer(value, label)?;
    BigNum::from_slice(&bytes).map_err(javascript_crypto_openssl_error)
}

fn javascript_crypto_curve_nid(name: &str) -> Result<Nid, SidecarError> {
    match name {
        "prime256v1" | "P-256" => Ok(Nid::X9_62_PRIME256V1),
        "secp384r1" | "P-384" => Ok(Nid::SECP384R1),
        "secp521r1" | "P-521" => Ok(Nid::SECP521R1),
        "secp256k1" => Ok(Nid::SECP256K1),
        other => Err(SidecarError::InvalidState(format!(
            "unsupported EC curve {other}"
        ))),
    }
}

fn javascript_crypto_named_dh_group(name: &str) -> Result<Dh<Params>, SidecarError> {
    match name {
        "modp2" => Dh::get_1024_160().map_err(javascript_crypto_openssl_error),
        "modp14" | "modp15" | "modp16" | "modp17" | "modp18" => {
            Dh::get_2048_256().map_err(javascript_crypto_openssl_error)
        }
        other => Err(SidecarError::InvalidState(format!(
            "unsupported Diffie-Hellman group {other}"
        ))),
    }
}

fn javascript_crypto_clone_dh_params(params: &Dh<Params>) -> Result<Dh<Params>, SidecarError> {
    Dh::from_pqg(
        params
            .prime_p()
            .to_owned()
            .map_err(javascript_crypto_openssl_error)?,
        params
            .prime_q()
            .map(|value| value.to_owned().map_err(javascript_crypto_openssl_error))
            .transpose()?,
        params
            .generator()
            .to_owned()
            .map_err(javascript_crypto_openssl_error)?,
    )
    .map_err(javascript_crypto_openssl_error)
}

fn javascript_crypto_build_dh_params(args: &[Value]) -> Result<Dh<Params>, SidecarError> {
    let Some(first) = args.first() else {
        return Err(SidecarError::InvalidState(String::from(
            "Diffie-Hellman session args are required",
        )));
    };
    if let Some(bits) = first.as_u64() {
        let generator = args
            .get(1)
            .map(|value| javascript_crypto_u32_from_bridge_value(value, "Diffie-Hellman generator"))
            .transpose()?
            .unwrap_or(2);
        return Dh::generate_params(bits as u32, generator)
            .map_err(javascript_crypto_openssl_error);
    }
    let prime = javascript_crypto_bignum_from_bridge_value(first, "Diffie-Hellman prime")?;
    let generator = args
        .get(1)
        .map(|value| javascript_crypto_bignum_from_bridge_value(value, "Diffie-Hellman generator"))
        .transpose()?
        .unwrap_or(BigNum::from_u32(2).map_err(javascript_crypto_openssl_error)?);
    Dh::from_pqg(prime, None, generator).map_err(javascript_crypto_openssl_error)
}

fn javascript_crypto_call_dh_session(
    session: &mut ActiveDhSession,
    method: &str,
    args: &[Value],
) -> Result<(Value, bool), SidecarError> {
    match method {
        "verifyError" => Ok((Value::Null, false)),
        "generateKeys" => {
            if session.key_pair.is_none() {
                session.key_pair = Some(
                    javascript_crypto_clone_dh_params(&session.params)?
                        .generate_key()
                        .map_err(javascript_crypto_openssl_error)?,
                );
            }
            let public = session
                .key_pair
                .as_ref()
                .expect("dh key pair")
                .public_key()
                .to_vec();
            Ok((javascript_crypto_bridge_buffer_value(&public), true))
        }
        "computeSecret" => {
            if session.key_pair.is_none() {
                session.key_pair = Some(
                    javascript_crypto_clone_dh_params(&session.params)?
                        .generate_key()
                        .map_err(javascript_crypto_openssl_error)?,
                );
            }
            let peer = javascript_crypto_bignum_from_bridge_value(
                args.first().ok_or_else(|| {
                    SidecarError::InvalidState(String::from(
                        "computeSecret requires peer public key",
                    ))
                })?,
                "Diffie-Hellman peer public key",
            )?;
            let private_key = session
                .key_pair
                .as_ref()
                .expect("dh key pair")
                .private_key();
            let mut secret = BigNum::new().map_err(javascript_crypto_openssl_error)?;
            let mut ctx = BigNumContext::new().map_err(javascript_crypto_openssl_error)?;
            secret
                .mod_exp(&peer, private_key, session.params.prime_p(), &mut ctx)
                .map_err(javascript_crypto_openssl_error)?;
            Ok((
                javascript_crypto_bridge_buffer_value(
                    &secret
                        .to_vec_padded(session.params.prime_p().num_bytes())
                        .map_err(javascript_crypto_openssl_error)?,
                ),
                true,
            ))
        }
        "getPrime" => Ok((
            javascript_crypto_bridge_buffer_value(&session.params.prime_p().to_vec()),
            true,
        )),
        "getGenerator" => Ok((
            javascript_crypto_bridge_buffer_value(&session.params.generator().to_vec()),
            true,
        )),
        "getPublicKey" => {
            if session.key_pair.is_none() {
                session.key_pair = Some(
                    javascript_crypto_clone_dh_params(&session.params)?
                        .generate_key()
                        .map_err(javascript_crypto_openssl_error)?,
                );
            }
            Ok((
                javascript_crypto_bridge_buffer_value(
                    &session
                        .key_pair
                        .as_ref()
                        .expect("dh key pair")
                        .public_key()
                        .to_vec(),
                ),
                true,
            ))
        }
        "getPrivateKey" => {
            if session.key_pair.is_none() {
                session.key_pair = Some(
                    javascript_crypto_clone_dh_params(&session.params)?
                        .generate_key()
                        .map_err(javascript_crypto_openssl_error)?,
                );
            }
            Ok((
                javascript_crypto_bridge_buffer_value(
                    &session
                        .key_pair
                        .as_ref()
                        .expect("dh key pair")
                        .private_key()
                        .to_vec(),
                ),
                true,
            ))
        }
        "setPrivateKey" => {
            let private_key = javascript_crypto_bignum_from_bridge_value(
                args.first().ok_or_else(|| {
                    SidecarError::InvalidState(String::from("setPrivateKey requires private key"))
                })?,
                "Diffie-Hellman private key",
            )?;
            let mut public_key = BigNum::new().map_err(javascript_crypto_openssl_error)?;
            let mut ctx = BigNumContext::new().map_err(javascript_crypto_openssl_error)?;
            public_key
                .mod_exp(
                    session.params.generator(),
                    &private_key,
                    session.params.prime_p(),
                    &mut ctx,
                )
                .map_err(javascript_crypto_openssl_error)?;
            session.key_pair = Some(
                javascript_crypto_clone_dh_params(&session.params)?
                    .set_key(public_key, private_key)
                    .map_err(javascript_crypto_openssl_error)?,
            );
            Ok((Value::Null, false))
        }
        "setPublicKey" => {
            let public_key = javascript_crypto_bignum_from_bridge_value(
                args.first().ok_or_else(|| {
                    SidecarError::InvalidState(String::from("setPublicKey requires public key"))
                })?,
                "Diffie-Hellman public key",
            )?;
            let private_key = session
                .key_pair
                .as_ref()
                .ok_or_else(|| {
                    SidecarError::InvalidState(String::from(
                        "setPublicKey requires private key to be set first",
                    ))
                })?
                .private_key()
                .to_owned()
                .map_err(javascript_crypto_openssl_error)?;
            session.key_pair = Some(
                javascript_crypto_clone_dh_params(&session.params)?
                    .set_key(public_key, private_key)
                    .map_err(javascript_crypto_openssl_error)?,
            );
            Ok((Value::Null, false))
        }
        other => Err(SidecarError::InvalidState(format!(
            "Unsupported Diffie-Hellman method: {other}"
        ))),
    }
}

fn javascript_crypto_call_ecdh_session(
    session: &mut ActiveEcdhSession,
    method: &str,
    args: &[Value],
) -> Result<(Value, bool), SidecarError> {
    let nid = javascript_crypto_curve_nid(&session.curve)?;
    let group = EcGroup::from_curve_name(nid).map_err(javascript_crypto_openssl_error)?;
    match method {
        "verifyError" => Ok((Value::Null, false)),
        "generateKeys" => {
            if session.key_pair.is_none() {
                session.key_pair =
                    Some(EcKey::generate(&group).map_err(javascript_crypto_openssl_error)?);
            }
            let mut ctx = BigNumContext::new().map_err(javascript_crypto_openssl_error)?;
            let bytes = session
                .key_pair
                .as_ref()
                .expect("ecdh key pair")
                .public_key()
                .to_bytes(&group, PointConversionForm::UNCOMPRESSED, &mut ctx)
                .map_err(javascript_crypto_openssl_error)?;
            Ok((javascript_crypto_bridge_buffer_value(&bytes), true))
        }
        "computeSecret" => {
            if session.key_pair.is_none() {
                session.key_pair =
                    Some(EcKey::generate(&group).map_err(javascript_crypto_openssl_error)?);
            }
            let peer_bytes = javascript_crypto_decode_bridge_buffer(
                args.first().ok_or_else(|| {
                    SidecarError::InvalidState(String::from(
                        "computeSecret requires peer public key",
                    ))
                })?,
                "ECDH peer public key",
            )?;
            let mut ctx = BigNumContext::new().map_err(javascript_crypto_openssl_error)?;
            let peer_point = EcPoint::from_bytes(&group, &peer_bytes, &mut ctx)
                .map_err(javascript_crypto_openssl_error)?;
            let peer_key = EcKey::from_public_key(&group, &peer_point)
                .map_err(javascript_crypto_openssl_error)?;
            let private =
                PKey::from_ec_key(session.key_pair.as_ref().expect("ecdh key pair").to_owned())
                    .map_err(javascript_crypto_openssl_error)?;
            let peer = PKey::from_ec_key(peer_key).map_err(javascript_crypto_openssl_error)?;
            let mut deriver = Deriver::new(&private).map_err(javascript_crypto_openssl_error)?;
            deriver
                .set_peer(&peer)
                .map_err(javascript_crypto_openssl_error)?;
            let secret = deriver
                .derive_to_vec()
                .map_err(javascript_crypto_openssl_error)?;
            Ok((javascript_crypto_bridge_buffer_value(&secret), true))
        }
        "getPublicKey" => {
            if session.key_pair.is_none() {
                session.key_pair =
                    Some(EcKey::generate(&group).map_err(javascript_crypto_openssl_error)?);
            }
            let mut ctx = BigNumContext::new().map_err(javascript_crypto_openssl_error)?;
            let bytes = session
                .key_pair
                .as_ref()
                .expect("ecdh key pair")
                .public_key()
                .to_bytes(&group, PointConversionForm::UNCOMPRESSED, &mut ctx)
                .map_err(javascript_crypto_openssl_error)?;
            Ok((javascript_crypto_bridge_buffer_value(&bytes), true))
        }
        "getPrivateKey" => {
            if session.key_pair.is_none() {
                session.key_pair =
                    Some(EcKey::generate(&group).map_err(javascript_crypto_openssl_error)?);
            }
            Ok((
                javascript_crypto_bridge_buffer_value(
                    &session
                        .key_pair
                        .as_ref()
                        .expect("ecdh key pair")
                        .private_key()
                        .to_vec(),
                ),
                true,
            ))
        }
        "setPrivateKey" => {
            let private_key = javascript_crypto_bignum_from_bridge_value(
                args.first().ok_or_else(|| {
                    SidecarError::InvalidState(String::from("setPrivateKey requires private key"))
                })?,
                "ECDH private key",
            )?;
            let mut ctx = BigNumContext::new().map_err(javascript_crypto_openssl_error)?;
            let mut public_key = EcPoint::new(&group).map_err(javascript_crypto_openssl_error)?;
            public_key
                .mul_generator2(&group, &private_key, &mut ctx)
                .map_err(javascript_crypto_openssl_error)?;
            session.key_pair = Some(
                EcKey::from_private_components(&group, &private_key, &public_key)
                    .map_err(javascript_crypto_openssl_error)?,
            );
            Ok((Value::Null, false))
        }
        "setPublicKey" => {
            let public_key_bytes = javascript_crypto_decode_bridge_buffer(
                args.first().ok_or_else(|| {
                    SidecarError::InvalidState(String::from("setPublicKey requires public key"))
                })?,
                "ECDH public key",
            )?;
            let mut ctx = BigNumContext::new().map_err(javascript_crypto_openssl_error)?;
            let public_key = EcPoint::from_bytes(&group, &public_key_bytes, &mut ctx)
                .map_err(javascript_crypto_openssl_error)?;
            let private_key = session
                .key_pair
                .as_ref()
                .ok_or_else(|| {
                    SidecarError::InvalidState(String::from(
                        "setPublicKey requires private key to be set first",
                    ))
                })?
                .private_key()
                .to_owned()
                .map_err(javascript_crypto_openssl_error)?;
            session.key_pair = Some(
                EcKey::from_private_components(&group, &private_key, &public_key)
                    .map_err(javascript_crypto_openssl_error)?,
            );
            Ok((Value::Null, false))
        }
        other => Err(SidecarError::InvalidState(format!(
            "Unsupported Diffie-Hellman method: {other}"
        ))),
    }
}

fn javascript_crypto_serialize_encoded_key_value_public(
    key: &PKey<Public>,
    encoding: Option<&Value>,
) -> Result<Value, SidecarError> {
    if let Some(encoding) = encoding {
        let format = encoding
            .get("format")
            .and_then(Value::as_str)
            .unwrap_or("pem");
        return Ok(match format {
            "der" => json!({
                "kind": "buffer",
                "value": base64::engine::general_purpose::STANDARD
                    .encode(key.public_key_to_der().map_err(javascript_crypto_openssl_error)?),
            }),
            _ => json!({
                "kind": "string",
                "value": String::from_utf8(
                    key.public_key_to_pem().map_err(javascript_crypto_openssl_error)?,
                )
                .map_err(|error| SidecarError::InvalidState(format!("public key PEM utf8: {error}")))?,
            }),
        });
    }
    javascript_crypto_serialize_sandbox_key_object(&JavascriptCryptoKeyMaterial::Public(
        key.to_owned(),
    ))
}

fn javascript_crypto_serialize_encoded_key_value_private(
    key: &PKey<Private>,
    encoding: Option<&Value>,
) -> Result<Value, SidecarError> {
    if let Some(encoding) = encoding {
        let format = encoding
            .get("format")
            .and_then(Value::as_str)
            .unwrap_or("pem");
        return Ok(match format {
            "der" => json!({
                "kind": "buffer",
                "value": base64::engine::general_purpose::STANDARD
                    .encode(key.private_key_to_der().map_err(javascript_crypto_openssl_error)?),
            }),
            _ => json!({
                "kind": "string",
                "value": String::from_utf8(
                    key.private_key_to_pem_pkcs8().map_err(javascript_crypto_openssl_error)?,
                )
                .map_err(|error| SidecarError::InvalidState(format!("private key PEM utf8: {error}")))?,
            }),
        });
    }
    javascript_crypto_serialize_sandbox_key_object(&JavascriptCryptoKeyMaterial::Private(
        key.to_owned(),
    ))
}

fn javascript_crypto_bridge_buffer_value(bytes: &[u8]) -> Value {
    bridge_buffer_value(bytes)
}

fn javascript_crypto_cipher_error(error: AesCipherError) -> SidecarError {
    SidecarError::InvalidState(error.0)
}

fn javascript_crypto_decode_cipher_option_b64(
    options: Option<&Value>,
    field: &str,
) -> Result<Option<Vec<u8>>, SidecarError> {
    let Some(encoded) = options
        .and_then(|value| value.get(field))
        .and_then(Value::as_str)
    else {
        return Ok(None);
    };
    base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .map(Some)
        .map_err(|error| {
            SidecarError::InvalidState(format!("cipher {field} contains invalid base64: {error}"))
        })
}

fn javascript_crypto_build_cipher_session(
    algorithm: &str,
    key: &[u8],
    iv: Option<&[u8]>,
    decrypt: bool,
    options: Option<&Value>,
) -> Result<StreamCipherSession, SidecarError> {
    let pad = options
        .and_then(|value| value.get("autoPadding"))
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let aad = if javascript_crypto_is_aead(algorithm) {
        javascript_crypto_decode_cipher_option_b64(options, "aad")?
    } else {
        None
    };
    let auth_tag = if decrypt && javascript_crypto_is_aead(algorithm) {
        javascript_crypto_decode_cipher_option_b64(options, "authTag")?
    } else {
        None
    };
    let tag_len = javascript_crypto_requested_aead_tag_len(algorithm, options)?;
    StreamCipherSession::new(
        algorithm,
        key,
        iv,
        decrypt,
        pad,
        aad.as_deref(),
        auth_tag.as_deref(),
        tag_len,
    )
    .map_err(javascript_crypto_cipher_error)
}

fn javascript_crypto_requested_aead_tag_len(
    algorithm: &str,
    options: Option<&Value>,
) -> Result<usize, SidecarError> {
    if !javascript_crypto_is_aead(algorithm) {
        return Ok(0);
    }
    let requested = options
        .and_then(|value| value.get("authTagLength"))
        .and_then(Value::as_u64)
        .unwrap_or(javascript_crypto_aead_tag_len(algorithm) as u64);
    usize::try_from(requested).map_err(|_| {
        SidecarError::InvalidState(String::from("cipher authTagLength must fit within usize"))
    })
}

fn javascript_crypto_cipher_update(
    session: &mut StreamCipherSession,
    data: &[u8],
) -> Result<Vec<u8>, SidecarError> {
    session.update(data).map_err(javascript_crypto_cipher_error)
}

fn javascript_crypto_is_aead(algorithm: &str) -> bool {
    crate::crypto_cipher::is_aead(algorithm)
}

fn javascript_crypto_aead_tag_len(_algorithm: &str) -> usize {
    crate::crypto_cipher::default_aead_tag_len()
}

fn javascript_crypto_openssl_error(error: openssl::error::ErrorStack) -> SidecarError {
    SidecarError::Execution(format!("crypto operation failed: {error}"))
}
