import * as vscode from 'vscode';
import * as path from 'path';

import {
	LanguageClient,
	LanguageClientOptions,
	ServerOptions,
	TransportKind,
	createClientSocketTransport
} from 'vscode-languageclient/node';

let client: LanguageClient;
export function activate(context: vscode.ExtensionContext) {

	console.log('Activating ARK language server extension');

	let disposable = vscode.commands.registerCommand('ark.helloWorld', () => {
		vscode.window.showInformationMessage('Hello World from ark!');
	});

	context.subscriptions.push(disposable);

	let serverOptions = () => {
		// TODO: port needs to be configurable or discoverable
		return createClientSocketTransport(9276).then(transport => {
			return transport.onConnected().then((protocol) => {
				return {
					reader: protocol[0],
					writer: protocol[1]
				};
			});
		});
	};

	let clientOptions: LanguageClientOptions = {
		documentSelector: [{ scheme: 'file', language: 'R' }],
	};

	client = new LanguageClient('ark', 'ARK Language Server', serverOptions, clientOptions);

	client.start();
}

export function deactivate() { }