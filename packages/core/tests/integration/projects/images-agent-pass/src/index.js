import { Jimp } from "jimp";
import { PNG } from "pngjs";
import jpeg from "jpeg-js";

const image = new Jimp({ width: 2, height: 2, color: 0xff0000ff });
image.setPixelColor(0x00ff00ff, 1, 1);
const jimpPng = await image.getBuffer("image/png");

const png = new PNG({ width: 2, height: 1 });
png.data.set([255, 0, 0, 255, 0, 255, 0, 255]);
const pngBytes = PNG.sync.write(png);
const pngRoundTrip = PNG.sync.read(pngBytes);

const jpegBytes = jpeg.encode({ data: Buffer.from([255,0,0,255, 0,255,0,255]), width: 2, height: 1 }, 90).data;
const jpegRoundTrip = jpeg.decode(jpegBytes, { useTArray: true });

console.log(JSON.stringify({
  jimp: [jimpPng[0], jimpPng[1], jimpPng.length > 20],
  png: [pngRoundTrip.width, pngRoundTrip.height, [...pngRoundTrip.data.slice(0, 4)]],
  jpeg: [jpegRoundTrip.width, jpegRoundTrip.height, jpegBytes.length > 20],
}));
