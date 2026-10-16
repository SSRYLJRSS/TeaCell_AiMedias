import { readFile, mkdir, writeFile } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';
import path from 'node:path';
import process from 'node:process';

const directory=path.dirname(fileURLToPath(import.meta.url));
export async function buildPrototype() {
  const root=path.resolve(directory,'../..');
  const [template,theme,logo]=await Promise.all([
    readFile(path.join(directory,'template.html'),'utf8'),
    readFile(path.join(root,'src/styles/theme.css'),'utf8'),
    readFile(path.join(root,'src/assets/icon-logo.png')),
  ]);
  const destination=path.join(root,'artifacts/assistant-ui/prototype.html');
  await mkdir(path.dirname(destination),{recursive:true});
  await writeFile(destination,template.replace('__THEME__',()=>theme.replace(/^\s*@theme\s*\{/m,':root {')).replaceAll('__LOGO__',`data:image/png;base64,${logo.toString('base64')}`));
  return destination;
}
if(process.argv[1] && path.resolve(process.argv[1])===fileURLToPath(import.meta.url)) {
  process.stdout.write(`${await buildPrototype()}\n`);
}
