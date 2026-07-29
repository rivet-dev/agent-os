import JSZip from "jszip";
import { ZipArchive } from "archiver";
import { PassThrough } from "node:stream";
import { strFromU8, strToU8, unzipSync, zipSync } from "fflate";

const zip = new JSZip();
zip.file("hello.txt", "hello agent");
const jszipBytes = await zip.generateAsync({ type: "uint8array" });
const reopened = await JSZip.loadAsync(jszipBytes);

const output = new PassThrough();
const chunks = [];
output.on("data", chunk => chunks.push(chunk));
const archiveDone = new Promise((resolve, reject) => {
  output.on("end", resolve);
  output.on("error", reject);
});
const archive = new ZipArchive({ zlib: { level: 1 } });
archive.on("error", error => output.destroy(error));
archive.pipe(output);
archive.append("payload", { name: "payload.txt" });
await archive.finalize();
await archiveDone;

const fflateBytes = zipSync({ "data.txt": strToU8("compressed") });
const fflateFiles = unzipSync(fflateBytes);
console.log(JSON.stringify({
  jszip: await reopened.file("hello.txt").async("string"),
  archiver: Buffer.concat(chunks).length > 20,
  fflate: strFromU8(fflateFiles["data.txt"]),
}));
