// Packs dist-artifact/app.js and app.css into one HTML page for the Claude artifact viewer,
// which supplies the document skeleton itself.
import { readFileSync, writeFileSync } from 'node:fs';

const dir = new URL('../dist-artifact/', import.meta.url);
const js = readFileSync(new URL('app.js', dir), 'utf8').replaceAll('</script', '<\\/script');
const css = readFileSync(new URL('app.css', dir), 'utf8').replaceAll('</style', '<\\/style');
const page = `<title>Constellation Rooms</title>
<style>${css}</style>
<div id="root"></div>
<script type="module">${js}</script>
`;
writeFileSync(new URL('constellation-rooms.html', dir), page);
console.log(`dist-artifact/constellation-rooms.html: ${(page.length / 1024).toFixed(0)} KB`);
