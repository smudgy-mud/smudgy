// A directory picker keeps source files local. Compilation and IndexedDB writes
// happen in a separate worker, then the selected profile refers to its name.
export function selectAndInstallPackage() {
  return new Promise((resolve, reject) => {
    const input = document.createElement('input');
    input.type = 'file';
    input.multiple = true;
    input.webkitdirectory = true;
    input.style.display = 'none';
    document.body.append(input);
    input.oncancel = () => {
      input.remove();
      resolve(null);
    };
    input.onchange = async () => {
      try {
        const picked = [...input.files];
        if (picked.length === 0) {
          resolve(null);
          return;
        }
        const paths = picked.map((file) => file.webkitRelativePath || file.name);
        const root = paths[0].split('/')[0];
        if (paths.some((path) => path.split('/')[0] !== root)) {
          throw new Error('Select a single package directory');
        }
        const files = await Promise.all(picked.map(async (file, index) => [
          paths[index].includes('/') ? paths[index].slice(root.length + 1) : paths[index],
          await file.text(),
        ]));
        const name = paths[0].includes('/') ? root : 'local';
        const worker = new Worker('package-install-worker.js', { type: 'module' });
        worker.onmessage = ({ data }) => {
          worker.terminate();
          if (data.error) reject(new Error(data.error));
          else resolve(data.name);
        };
        worker.onerror = (event) => {
          worker.terminate();
          reject(new Error(event.message));
        };
        worker.postMessage({ name, files });
      } catch (error) {
        reject(error);
      } finally {
        input.remove();
      }
    };
    input.click();
  });
}
