import { ALLOWED_BASES, normalizeBase } from "./lib/config.js";

const select = document.getElementById("base");
for (const b of ALLOWED_BASES) select.append(new Option(b, b));

const { base } = await chrome.storage.sync.get("base");
select.value = normalizeBase(base);

select.addEventListener("change", async () => {
  await chrome.storage.sync.set({ base: normalizeBase(select.value) });
  document.getElementById("saved").hidden = false;
});
