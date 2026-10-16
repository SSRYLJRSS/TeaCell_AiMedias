import http from 'node:http';
import { readFile } from 'node:fs/promises';
import { buildPrototype } from './build.mjs';
import process from 'node:process';

const documentPath=await buildPrototype();
const portIndex=process.argv.indexOf('--port');
const port=portIndex<0?0:Number(process.argv[portIndex+1]);
if(!Number.isInteger(port)||port<0||port>65535)throw new Error('Expected --port to be an integer from 0 to 65535.');
const server=http.createServer(async(request,response)=>{
  if(request.url!=='/' || request.method!=='GET') {response.writeHead(404);response.end();return;}
  response.writeHead(200,{'Content-Type':'text/html; charset=utf-8','Content-Security-Policy':"default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; img-src data:; connect-src 'none'; base-uri 'none'; form-action 'none'"});
  response.end(await readFile(documentPath));
});
server.listen(port,'127.0.0.1',()=>process.stdout.write(`http://127.0.0.1:${server.address().port}/\n`));
