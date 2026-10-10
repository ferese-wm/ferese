// Progressive enhancement for the handbook. Navigation and content work without JS.
const handbookNavigation = document.querySelector('.handbook-navigation');
if (handbookNavigation) {
  const compactLayout = matchMedia('(max-width: 900px)');
  const updateNavigation = () => { handbookNavigation.open = !compactLayout.matches; };
  updateNavigation();
  compactLayout.addEventListener('change', updateNavigation);
  handbookNavigation.addEventListener('click', (event) => {
    if (compactLayout.matches && event.target.closest('nav a')) handbookNavigation.open = false;
  });
}
const search = document.querySelector("#docs-search");
const results = document.querySelector("#search-results");
const status = document.querySelector("#search-status");
let indexPromise;
if (search) {
  search.closest(".docs-search").hidden = false;
  const loadIndex = () =>
    (indexPromise ||= fetch("search-index.json")
      .then((response) => {
        if (!response.ok) throw new Error("Search index unavailable");
        return response.json();
      })
      .catch((error) => {
        indexPromise = null;
        throw error;
      }));
  search.addEventListener("focus", () => {
    loadIndex().catch(() => {});
  });
  search.addEventListener("input", async () => {
    const query = search.value.trim().toLocaleLowerCase();
    if (!query) {
      results.hidden = true;
      results.replaceChildren();
      status.textContent = "";
      return;
    }
    try {
      const index = await loadIndex();
      if (search.value.trim().toLocaleLowerCase() !== query) return;
      const words = query.split(/\s+/);
      const matches = index
        .filter((item) =>
          words.every((word) =>
            `${item.page} ${item.heading} ${item.text}`
              .toLocaleLowerCase()
              .includes(word),
          ),
        )
        .sort(
          (a, b) =>
            Number(b.heading.toLocaleLowerCase().includes(query)) -
            Number(a.heading.toLocaleLowerCase().includes(query)),
        )
        .slice(0, 10);
      results.replaceChildren();
      for (const item of matches) {
        const link = document.createElement("a");
        link.href = item.url;
        const title = document.createElement("strong");
        title.textContent = `${item.page} / ${item.heading}`;
        const snippet = document.createElement("span");
        const position = Math.max(
          0,
          item.text.toLocaleLowerCase().indexOf(words[0]) - 45,
        );
        snippet.textContent = `${position ? "…" : ""}${item.text.slice(position, position + 165)}${item.text.length > position + 165 ? "…" : ""}`;
        link.append(title, snippet);
        results.append(link);
      }
      if (!matches.length) {
        const empty = document.createElement("p");
        empty.textContent =
          "No matching sections. Try “wallpaper”, “shortcuts”, or “notifications”.";
        results.append(empty);
      }
      results.hidden = false;
      status.textContent = `${matches.length} matching sections.`;
    } catch {
      if (search.value.trim().toLocaleLowerCase() !== query) return;
      results.hidden = true;
      status.textContent = "Search is unavailable. Browse the guides below.";
    }
  });
  document.addEventListener("keydown", (event) => {
    if (event.key === "Escape") {
      results.hidden = true;
      search.focus();
    }
    if (
      event.key === "/" &&
      !["INPUT", "TEXTAREA"].includes(document.activeElement.tagName) &&
      !event.ctrlKey &&
      !event.metaKey &&
      !event.altKey
    ) {
      event.preventDefault();
      search.focus();
    }
    if (
      event.key === "ArrowDown" &&
      document.activeElement === search &&
      !results.hidden
    ) {
      const first = results.querySelector("a");
      if (first) {
        event.preventDefault();
        first.focus();
      }
    }
  });
  document.addEventListener("click", (event) => {
    if (!event.target.closest(".docs-search")) results.hidden = true;
  });
}
if (navigator.clipboard?.writeText) {
  for (const block of document.querySelectorAll(".prose pre")) {
    const button = document.createElement("button");
    button.type = "button";
    button.className = "code-copy";
    button.textContent = "Copy";
    button.setAttribute("aria-label", "Copy code example");
    button.addEventListener("click", async () => {
      try {
        await navigator.clipboard.writeText(
          block.querySelector("code").textContent,
        );
        button.textContent = "Copied";
        if (status) status.textContent = "Code example copied.";
      } catch {
        button.textContent = "Select to copy";
        if (status)
          status.textContent =
            "Copy failed. Select the code to copy it manually.";
      }
    });
    block.append(button);
  }
}
