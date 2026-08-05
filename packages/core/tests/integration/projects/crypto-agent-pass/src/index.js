import { SignJWT, jwtVerify } from "jose";
import * as openpgp from "openpgp";
import sodium from "libsodium-wrappers";
import nacl from "tweetnacl";

const key = new TextEncoder().encode("01234567890123456789012345678901");
const token = await new SignJWT({ role: "agent" }).setProtectedHeader({ alg: "HS256" }).sign(key);
const verified = await jwtVerify(token, key);

const encrypted = await openpgp.encrypt({
  message: await openpgp.createMessage({ text: "secret" }),
  passwords: ["passphrase"],
  format: "armored",
});
const decrypted = await openpgp.decrypt({ message: await openpgp.readMessage({ armoredMessage: encrypted }), passwords: ["passphrase"], format: "utf8" });

await sodium.ready;
const sodiumKey = new Uint8Array(sodium.crypto_secretbox_KEYBYTES).fill(7);
const sodiumNonce = new Uint8Array(sodium.crypto_secretbox_NONCEBYTES).fill(3);
const sodiumCipher = sodium.crypto_secretbox_easy(sodium.from_string("sodium"), sodiumNonce, sodiumKey);
const sodiumPlain = sodium.to_string(sodium.crypto_secretbox_open_easy(sodiumCipher, sodiumNonce, sodiumKey));

const naclKey = new Uint8Array(nacl.secretbox.keyLength).fill(9);
const naclNonce = new Uint8Array(nacl.secretbox.nonceLength).fill(4);
const naclCipher = nacl.secretbox(new TextEncoder().encode("nacl"), naclNonce, naclKey);
const naclPlain = new TextDecoder().decode(nacl.secretbox.open(naclCipher, naclNonce, naclKey));

console.log(JSON.stringify([verified.payload.role, decrypted.data, sodiumPlain, naclPlain]));
