(function (root, pkg_path, output_name, wasm_output_name) {
	const wasm_url = `${root}/${pkg_path}/${wasm_output_name}.wasm`;
	// Start the WASM download right away and hand the pending Response to
	// wasm-bindgen's `init`, which accepts a `Promise<Response>`. This replaces
	// `<link rel="preload" as="fetch">`: WebKit does not match that preload
	// against the `fetch()` wasm-bindgen performs, so Safari downloaded the
	// binary twice. Starting the request here guarantees exactly one download
	// in every browser, and starts it as early as the preload did because this
	// script runs synchronously while `<head>` is being parsed.
	const wasm = fetch(wasm_url);
	// mark as handled so a failed fetch does not also surface as an
	// "Unhandled Promise Rejection" (it is reported by the catch below)
	wasm.catch(() => {});
	// hydration walks the DOM, so wait for the document to be fully parsed
	const dom_ready =
		document.readyState === "loading"
			? new Promise((resolve) =>
					document.addEventListener("DOMContentLoaded", resolve, { once: true }),
				)
			: Promise.resolve();
	Promise.all([import(`${root}/${pkg_path}/${output_name}.js`), dom_ready])
		.then(([mod]) =>
			mod.default({ module_or_path: wasm }).then(() => {
				mod.hydrate();
			}),
		)
		.catch((err) => {
			// Navigating away while the WASM is still loading cancels the fetch
			// and rejects the promise (WebKit: "TypeError: Load failed"); that
			// is harmless, so keep it to a single concise warning.
			console.warn(
				`[halyard] hydration did not run: ${err && err.message ? err.message : err} (${wasm_url})`,
			);
		});
})
