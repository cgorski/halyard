((root, pkg_path, output_name, wasm_output_name) => {
	let MOST_RECENT_CHILDREN_CB = [];

	function idle(c) {
		if ("requestIdleCallback" in window) {
			window.requestIdleCallback(c);
		} else {
			c();
		}
	}
	async function hydrateIslands(rootNode, mod) {
		async function traverse(node) {
			if (node.nodeType === Node.ELEMENT_NODE) {
				const tag = node.tagName.toLowerCase();
				if(tag === 'halyard-island') {
					const children = [];
					const id = node.dataset.component || null;

					await hydrateIsland(node, id, mod);
					
					for(const child of node.children) {
						await traverse(child, children);
					}
				} else {
					if (tag === 'halyard-children') {
						MOST_RECENT_CHILDREN_CB.push(node.$$on_hydrate);
						for(const child of node.children) {
							await traverse(child);
						};
						// un-set the "most recent children"
						MOST_RECENT_CHILDREN_CB.pop();
					} else {
						for(const child of node.children) {
							await traverse(child);
						};
					}
				}
			}
		}

		await traverse(rootNode);
	}
	async function hydrateIsland(el, id, mod) {
		const islandFn = mod[id];
		if (islandFn) {
			const children_cb = MOST_RECENT_CHILDREN_CB[MOST_RECENT_CHILDREN_CB.length-1];
			if (children_cb) {
				children_cb();
			}
			const res = islandFn(el);
			if (res && res.then) {
				await res;
			}
		} else {
			console.warn(`Could not find WASM function for the island ${id}.`);
		}
	}
	const wasm_url = `${root}/${pkg_path}/${wasm_output_name}.wasm`;
	// see hydration_script.js: one download, started immediately, in every browser
	const wasm = fetch(wasm_url);
	wasm.catch(() => {});
	const dom_ready =
		document.readyState === "loading"
			? new Promise((resolve) =>
					document.addEventListener("DOMContentLoaded", resolve, { once: true }),
				)
			: Promise.resolve();
	dom_ready.then(() => idle(() => {
		import(`${root}/${pkg_path}/${output_name}.js`)
			.then(mod => {
				window.__hydrateIsland = (el, id) => hydrateIsland(el, id, mod);
				return mod.default({module_or_path: wasm}).then(() => {
					mod.hydrate();
					return hydrateIslands(document.body, mod);
				});
			})
			.catch((err) => {
				console.warn(
					`[halyard] island hydration did not run: ${err && err.message ? err.message : err} (${wasm_url})`,
				);
			});
	}));
})
