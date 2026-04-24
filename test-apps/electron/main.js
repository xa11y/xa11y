const { app, BrowserWindow } = require('electron');
const path = require('path');

function getCliArg(name, fallback) {
  const prefix = `--${name}=`;
  for (const arg of process.argv) {
    if (arg.startsWith(prefix)) return arg.slice(prefix.length);
  }
  return fallback;
}

const APP_NAME = getCliArg('xa11y-app-name', 'xa11y-electron-test-app');

// Set the AT-SPI / process accessible name so each test fixture instance
// can be located unambiguously via `xa11y.App.by_name(...)`.
app.setName(APP_NAME);

// Enable accessibility for web content so AT-SPI/AX APIs can read the DOM.
app.commandLine.appendSwitch('force-renderer-accessibility');

function createWindow() {
  const win = new BrowserWindow({
    width: 800,
    height: 600,
    title: APP_NAME,
    webPreferences: {
      contextIsolation: true,
      nodeIntegration: false,
    },
  });
  win.loadFile(path.join(__dirname, 'index.html'));
}

app.commandLine.appendSwitch('no-sandbox');

app.whenReady().then(() => {
  createWindow();
});

app.on('window-all-closed', () => {
  app.quit();
});
