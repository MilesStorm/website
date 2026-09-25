import { ALLOWED_BASES, isOptional, normalizeBase, originPattern } from "./lib/config.js";

const select = document.getElementById("base");
for (const b of ALLOWED_BASES) select.append(new Option(b, b));

const { base } = await chrome.storage.sync.get("base");
let current = normalizeBase(base);
select.value = current;

select.addEventListener("change", async () => {
  const chosen = normalizeBase(select.value);
  // A dev server needs its own permission; Chrome asks the user (the change
  // event counts as the required user gesture).
  if (isOptional(chosen) && !(await chrome.permissions.request({ origins: [originPattern(chosen)] }))) {
    select.value = current;
    return;
  }
  current = chosen;
  await chrome.storage.sync.set({ base: chosen });
  document.getElementById("saved").hidden = false;
});
