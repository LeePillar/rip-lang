// Rip VSCode 扩展：启动 `rip lsp` 与 `rip dap`。
const vscode = require('vscode');

let client;

function ripExecutable() {
    return vscode.workspace.getConfiguration('rip').get('executable') || 'rip';
}

function activate(context) {
    const { LanguageClient, TransportKind } = require('vscode-languageclient/node');
    const command = ripExecutable();
    client = new LanguageClient(
        'rip',
        'Rip Language Server',
        { command, args: ['lsp'], transport: TransportKind.stdio },
        { documentSelector: [{ language: 'rip' }] }
    );
    context.subscriptions.push(client.start());

    context.subscriptions.push(
        vscode.debug.registerDebugAdapterDescriptorFactory('rip', {
            createDebugAdapterDescriptor() {
                return new vscode.DebugAdapterExecutable(ripExecutable(), ['dap']);
            }
        })
    );

    context.subscriptions.push(
        vscode.debug.registerDebugConfigurationProvider('rip', {
            resolveDebugConfiguration(_folder, config) {
                if (!config.program) {
                    config.program = '${file}';
                }
                if (!Array.isArray(config.args)) {
                    config.args = [];
                }
                return config;
            }
        })
    );
}

function deactivate() {
    return client ? client.stop() : undefined;
}

module.exports = { activate, deactivate };
