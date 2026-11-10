import * as path from "node:path";
import * as vscode from "vscode";
import {
  type Executable,
  LanguageClient,
  type LanguageClientOptions,
  type ServerOptions,
  TransportKind,
} from "vscode-languageclient/node";

const LANGUAGE_ID = "hexpat";
const DEBUG_TYPE = "hexa-template";
const SECTION = "hexaTemplates";

let client: LanguageClient | undefined;
let output: vscode.OutputChannel | undefined;

/** Resolves a configured binary: bare names stay as-is (looked up on PATH). */
function resolveBinary(setting: string, fallback: string): string {
  const raw = vscode.workspace.getConfiguration(SECTION).get<string>(setting)?.trim() || fallback;
  let value = raw;
  if (value.startsWith("~/") || value === "~") {
    value = path.join(process.env.HOME ?? process.env.USERPROFILE ?? "", value.slice(1));
  }
  const folder = vscode.workspace.workspaceFolders?.[0]?.uri.fsPath;
  if (folder) {
    value = value.replace(/\$\{workspaceFolder\}/g, folder);
  }
  const hasSeparator = value.includes("/") || value.includes("\\");
  if (hasSeparator && !path.isAbsolute(value) && folder) {
    value = path.join(folder, value);
  }
  return value;
}

async function startClient(): Promise<void> {
  const command = resolveBinary("serverPath", "hexa-template-lsp");
  const executable: Executable = { command, transport: TransportKind.stdio };
  const serverOptions: ServerOptions = { run: executable, debug: executable };
  const clientOptions: LanguageClientOptions = {
    documentSelector: [
      { scheme: "file", language: LANGUAGE_ID },
      { scheme: "untitled", language: LANGUAGE_ID },
    ],
    outputChannel: output,
  };
  client = new LanguageClient(SECTION, "Hexa Templates", serverOptions, clientOptions);
  try {
    await client.start();
  } catch (err) {
    const message = err instanceof Error ? err.message : String(err);
    const choice = await vscode.window.showErrorMessage(
      `Hexa Templates: cannot start language server "${command}": ${message}. ` +
        "Build it with `cargo build --release -p hexa-template --bins` and put it on PATH " +
        "or set hexaTemplates.serverPath.",
      "Open Settings",
    );
    if (choice === "Open Settings") {
      void vscode.commands.executeCommand("workbench.action.openSettings", `${SECTION}.serverPath`);
    }
    client = undefined;
  }
}

async function stopClient(): Promise<void> {
  const current = client;
  client = undefined;
  if (current) {
    try {
      await current.stop();
    } catch {
      // The server may already be gone.
    }
  }
}

function absolutize(value: unknown, folder: vscode.WorkspaceFolder | undefined): unknown {
  if (typeof value !== "string" || value === "" || path.isAbsolute(value)) {
    return value;
  }
  const base = folder?.uri.fsPath ?? vscode.workspace.workspaceFolders?.[0]?.uri.fsPath;
  return base ? path.join(base, value) : value;
}

async function pickDataFile(): Promise<string | undefined> {
  const picked = await vscode.window.showOpenDialog({
    canSelectMany: false,
    openLabel: "Apply template",
    title: "Data file for the template",
  });
  return picked?.[0]?.fsPath;
}

class ConfigurationProvider implements vscode.DebugConfigurationProvider {
  async resolveDebugConfiguration(
    _folder: vscode.WorkspaceFolder | undefined,
    config: vscode.DebugConfiguration,
  ): Promise<vscode.DebugConfiguration | undefined> {
    // F5 without a launch.json: debug the active template.
    if (!config.type && !config.request && !config.name) {
      const editor = vscode.window.activeTextEditor;
      if (editor?.document.languageId !== LANGUAGE_ID) {
        return config;
      }
      config.type = DEBUG_TYPE;
      config.request = "launch";
      config.name = "Debug template";
      config.template = editor.document.uri.fsPath;
      config.stopOnEntry = true;
    }
    if (!config.template) {
      const editor = vscode.window.activeTextEditor;
      if (editor?.document.languageId === LANGUAGE_ID && !editor.document.isUntitled) {
        config.template = editor.document.uri.fsPath;
      }
    }
    if (!config.data) {
      const data = await pickDataFile();
      if (!data) {
        return undefined;
      }
      config.data = data;
    }
    return config;
  }

  async resolveDebugConfigurationWithSubstitutedVariables(
    folder: vscode.WorkspaceFolder | undefined,
    config: vscode.DebugConfiguration,
  ): Promise<vscode.DebugConfiguration | undefined> {
    // The adapter opens paths relative to its own working directory.
    config.template = absolutize(config.template, folder);
    config.data = absolutize(config.data, folder);
    if (!config.template) {
      void vscode.window.showErrorMessage(
        "Hexa Templates: the launch configuration needs `template`.",
      );
      return undefined;
    }
    const doc = vscode.workspace.textDocuments.find((d) => d.uri.fsPath === config.template);
    if (doc?.isDirty) {
      await doc.save();
    }
    return config;
  }
}

class AdapterFactory implements vscode.DebugAdapterDescriptorFactory {
  createDebugAdapterDescriptor(
    session: vscode.DebugSession,
  ): vscode.ProviderResult<vscode.DebugAdapterDescriptor> {
    const command = resolveBinary("debugAdapterPath", "hexa-template-dap");
    const cwd = session.workspaceFolder?.uri.fsPath;
    return new vscode.DebugAdapterExecutable(command, [], cwd ? { cwd } : undefined);
  }
}

export async function activate(context: vscode.ExtensionContext): Promise<void> {
  output = vscode.window.createOutputChannel("Hexa Templates");
  context.subscriptions.push(output);

  context.subscriptions.push(
    vscode.debug.registerDebugConfigurationProvider(DEBUG_TYPE, new ConfigurationProvider()),
    vscode.debug.registerDebugAdapterDescriptorFactory(DEBUG_TYPE, new AdapterFactory()),
    vscode.commands.registerCommand(`${SECTION}.restartServer`, async () => {
      await stopClient();
      await startClient();
    }),
    vscode.commands.registerCommand(`${SECTION}.debugFile`, async (uri?: vscode.Uri) => {
      const template = uri?.fsPath ?? vscode.window.activeTextEditor?.document.uri.fsPath;
      if (!template) {
        return;
      }
      const data = await pickDataFile();
      if (!data) {
        return;
      }
      await vscode.debug.startDebugging(
        vscode.workspace.getWorkspaceFolder(vscode.Uri.file(template)),
        {
          type: DEBUG_TYPE,
          request: "launch",
          name: `Debug ${path.basename(template)}`,
          template,
          data,
          stopOnEntry: true,
        },
      );
    }),
    vscode.workspace.onDidChangeConfiguration(async (e) => {
      if (e.affectsConfiguration(`${SECTION}.serverPath`)) {
        await stopClient();
        await startClient();
      }
    }),
  );

  await startClient();
}

export async function deactivate(): Promise<void> {
  await stopClient();
}
