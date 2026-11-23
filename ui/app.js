"use strict";

const $ = id => document.getElementById(id);
const bridge = window.webkit?.messageHandlers?.framekeep;
const send = (op, params = {}) => bridge.postMessage({ op, ...params });
const defaultEdits = {
  exposure: 0, contrast: 0, saturation: 0, warmth: 0, highlights: 0, shadows: 0,
  rotation: 0, grade: "original", strength: 100,
};
const categories = {
  unreviewed: "Unreviewed",
  keep: "Keep",
  delete: "Delete",
  favorites: "Favorites",
};
const categoryEmpty = {
  unreviewed: ["You’re caught up", "Photos you mark Keep or Delete leave Unreviewed."],
  keep: ["No kept photos", "Use Keep to save photos in this view."],
  delete: ["Nothing marked for deletion", "Photos you mark Delete wait here until you recycle them."],
  favorites: ["No favorites yet", "Use the heart to add a photo to Favorites and Keep."],
};

function loadSetting(key, fallback) {
  try { return localStorage.getItem(key) ?? fallback; }
  catch { return fallback; }
}

function saveSetting(key, value) {
  try { localStorage.setItem(key, value); }
  catch {}
}

const state = {
  photos: [], visible: [], selected: null, filter: "unreviewed",
  root: null, name: "", generation: 0, skipped: 0,
  busy: "", canUndo: false, canUndoTrash: false, editor: null,
  view: loadSetting("framekeep-layout", "viewer") === "grid" ? "grid" : "viewer",
};
const filterButtons = [...document.querySelectorAll("[data-filter]")];
const openButtons = [...document.querySelectorAll("[data-open]")];
const viewButtons = [...document.querySelectorAll("[data-view]")];
const editInputs = [...document.querySelectorAll("[data-edit]")];
const preview = $("preview");
const scroll = $("film-scroll");
const track = $("film-track");
const gridScroll = $("grid-scroll");
const gridTrack = $("grid-track");
const gridSize = $("grid-size");
const tiles = new Map();
const cards = new Map();
const stride = 113;
let previewKey;
let shownOwner;
let pendingPreview;
let toastTimer;
let draftTimer;
let pendingEditsTimer;
const lutGrades = new Set();
let scheduledLayout = false;
let resetScroll = false;

gridSize.value = loadSetting("framekeep-grid-size", "180");

const selectedPhoto = () => state.photos[state.selected];
const countLabel = (number, unit) => number.toLocaleString() + " " + unit + (number === 1 ? "" : "s");
const matches = (photo, category) => category === "favorites" ? photo.favorite : photo.review === category;
const editsFor = photo => ({ ...defaultEdits, ...photo.edits?.[photo.format] });
const developedFor = photo => photo.developed ?? "png";

function notify(message, { error = false, undo = false } = {}) {
  clearTimeout(toastTimer);
  $("toast-text").textContent = message;
  $("toast").classList.toggle("error", error);
  $("toast-undo").hidden = !undo;
  $("toast").hidden = false;
  if (!error) toastTimer = setTimeout(() => { $("toast").hidden = true; }, 6500);
}

function previewURL(photo, size, edits = editsFor(photo)) {
  const base = "framekeep-preview://library/" + state.generation + "/" + photo.id + "/" + photo.format + "/" + size;
  return Object.keys(defaultEdits).some(key => edits[key] !== defaultEdits[key])
    ? base + "?edits=" + encodeURIComponent(JSON.stringify(edits)) : base;
}

function setFolder(result, rescan = false) {
  if (result.warning) notify(result.warning, { error: true });
  if (!("photos" in result)) return;
  const prior = rescan ? selectedPhoto() : null;
  state.root = result.root;
  state.name = result.name;
  state.generation = result.generation;
  state.photos = result.photos;
  state.skipped = result.skipped;
  state.canUndo = false;
  state.canUndoTrash = false;
  $("toast-undo").hidden = true;
  state.selected = prior ? (result.photos.find(photo =>
    photo.folder === prior.folder && photo.name === prior.name)?.id ?? null) : null;
  if (!rescan) state.filter = "unreviewed";
  resetPreviews();
}

function computeLibrary() {
  const counts = { unreviewed: 0, keep: 0, delete: 0, favorites: 0 };
  let files = 0;
  state.visible = [];
  for (const photo of state.photos) {
    if (!photo) continue;
    counts[photo.review]++;
    if (photo.favorite) counts.favorites++;
    files += [photo.png, photo.raf, photo.hif, photo.jpg].filter(Boolean).length;
    if (matches(photo, state.filter)) state.visible.push(photo);
  }
  if (!state.visible.some(photo => photo.id === state.selected)) {
    state.selected = state.visible[0]?.id ?? null;
  }
  for (const key of Object.keys(counts)) $(key + "-count").textContent = counts[key].toLocaleString();
  $("subtitle").textContent = countLabel(state.visible.length, "photo") + " in " + (state.name || "/");
  $("source-files").textContent = countLabel(files, "file");
}

function paint({ reveal = false, recompute = true } = {}) {
  if (recompute) computeLibrary();
  const photo = selectedPhoto();
  const locked = Boolean(state.busy);
  const editing = Boolean(state.editor);
  const opening = state.busy === "Opening folder…" || state.busy === "Scanning folder…";
  const onGrid = Boolean(photo && state.view === "grid");
  const deleteView = state.filter === "delete";
  const keepView = state.filter === "keep";
  $("heading").textContent = state.root ? categories[state.filter] : "Photo library";
  $("subtitle").hidden = !state.root;
  $("welcome").hidden = Boolean(state.root);
  $("folder-section").hidden = !state.root;
  $("source-name").textContent = state.name;
  $("source-name").title = state.root || "";
  $("footer-status").textContent = state.busy || (state.skipped ? countLabel(state.skipped, "unreadable item") + " skipped" : "Ready");
  $("refresh").disabled = locked || editing;
  $("toast-undo").disabled = locked || editing;
  for (const button of openButtons) button.disabled = locked || editing;
  for (const button of filterButtons) {
    const active = button.dataset.filter === state.filter;
    button.classList.toggle("active", active);
    if (active) button.setAttribute("aria-current", "page");
    else button.removeAttribute("aria-current");
    button.disabled = !state.root || locked || editing;
  }
  $("view-mode").hidden = !state.root;
  for (const button of viewButtons) {
    button.disabled = locked || editing;
    button.setAttribute("aria-pressed", String(button.dataset.view === state.view));
  }
  $("recycle-all").hidden = !state.root || !deleteView;
  $("recycle-all").disabled = !photo || locked || editing;
  $("trash-kept-raws").hidden = !state.root || !keepView;
  $("trash-kept-raws").disabled = locked || editing || !state.visible.some(item => item.raf);
  $("header-undo").hidden = !(deleteView || keepView) || !state.canUndoTrash;
  $("header-undo").disabled = locked || editing;

  $("empty").hidden = !state.root || Boolean(photo);
  $("empty-action").disabled = locked;
  $("review").hidden = !photo || onGrid;
  $("grid").hidden = !onGrid;
  if (!photo) {
    const hasFiles = state.photos.some(Boolean);
    const empty = hasFiles ? categoryEmpty[state.filter] : ["No photos here", "Choose a folder containing PNG, Fujifilm RAF, or HIF files."];
    $("empty-title").textContent = empty[0];
    $("empty-copy").textContent = empty[1];
    const emptyAction = $("empty-action");
    emptyAction.textContent = deleteView && state.canUndoTrash ? "Undo Trash" : hasFiles ? "Go to Unreviewed" : "Open a folder";
    emptyAction.hidden = hasFiles && (state.filter === "unreviewed" || (deleteView && !state.canUndoTrash));
    clearPreview();
    return;
  }

  const index = state.visible.findIndex(item => item.id === photo.id);
  if (onGrid) {
    clearPreview();
    paintGrid(reveal ? index : null);
    return;
  }
  const filename = photo[photo.format];
  $("file-name").textContent = filename;
  $("file-name").title = filename;
  $("file-folder").textContent = photo.folder;
  $("file-folder").title = photo.folder;
  $("position").textContent = (index + 1).toLocaleString() + " / " + state.visible.length.toLocaleString();
  $("previous").disabled = opening || editing || index === 0;
  $("next").disabled = opening || editing || index === state.visible.length - 1;
  $("undo").hidden = !state.canUndoTrash || deleteView || keepView;
  $("undo").disabled = locked || editing;
  $("keep").disabled = locked || editing || photo.review === "keep";
  $("keep").setAttribute("aria-pressed", String(photo.review === "keep"));
  $("favorite").disabled = locked || editing;
  $("favorite").setAttribute("aria-pressed", String(photo.favorite));
  const favoriteLabel = photo.favorite ? "Remove from favorites" : "Add to favorites";
  $("favorite").setAttribute("aria-label", favoriteLabel);
  $("favorite").title = favoriteLabel + " (H)";
  $("trash").disabled = locked || editing || photo.review === "delete";
  $("trash").setAttribute("aria-pressed", String(photo.review === "delete"));
  $("unreview").hidden = photo.review === "unreviewed";
  $("unreview").disabled = locked || editing;
  const raw = photo.format === "raf";
  const rawButton = $("view-raw");
  const developedButton = $("view-developed");
  rawButton.disabled = locked || editing || !photo.raf;
  rawButton.title = photo.raf ? "Show RAW (V)" : "No RAW file for this photo";
  rawButton.setAttribute("aria-pressed", String(raw));
  developedButton.disabled = locked || editing;
  developedButton.title = photo.developed ? "Show developed " + developedFor(photo).toUpperCase() + " (V)" : "Create and show a PNG copy (V)";
  developedButton.setAttribute("aria-pressed", String(!raw));
  const developedOptions = $("developed-format");
  developedOptions.hidden = raw || !photo.hif || !photo.png;
  developedOptions.disabled = locked || editing;
  developedOptions.value = developedFor(photo);
  $("edit").disabled = locked;
  $("edit").setAttribute("aria-expanded", String(editing));
  $("editor").hidden = !editing;
  if (editing) paintEditor();
  showPreview(photo);
  updateDragTarget();
  track.style.width = Math.max(0, state.visible.length * stride - 9) + "px";
  if (resetScroll) {
    scroll.scrollLeft = 0;
    resetScroll = false;
  }
  if (reveal) revealTile(index);
  paintFilmstrip();
}

function clearPreview() {
  if (!previewKey) return;
  previewKey = null;
  shownOwner = null;
  cancelPendingPreview();
  preview.hidden = true;
  preview.removeAttribute("src");
  updateDragTarget();
}

function cancelPendingPreview() {
  if (!pendingPreview) return;
  pendingPreview.onload = null;
  pendingPreview.onerror = null;
  pendingPreview.removeAttribute("src");
  pendingPreview = null;
}

function showPreview(photo) {
  const editor = state.editor;
  const edits = editor ? (editor.original ? defaultEdits : editor.edits) : editsFor(photo);
  const url = previewURL(photo, "large", edits);
  if (url === previewKey) return;
  previewKey = url;
  cancelPendingPreview();
  const owner = state.generation + "/" + photo.id + "/" + photo.format;
  if (shownOwner !== owner) {
    preview.hidden = true;
    $("preview-status").textContent = "Loading preview…";
    $("drag-hint").style.visibility = "hidden";
  }
  const candidate = new Image();
  pendingPreview = candidate;
  candidate.onload = () => {
    if (previewKey !== url) return;
    pendingPreview = null;
    shownOwner = owner;
    preview.alt = photo[photo.format];
    preview.src = url;
    preview.hidden = false;
    $("preview-status").textContent = "";
    $("drag-hint").style.visibility = "visible";
    updateDragTarget();
  };
  candidate.onerror = () => {
    if (previewKey !== url) return;
    pendingPreview = null;
    shownOwner = null;
    preview.hidden = true;
    $("preview-status").textContent = "Preview unavailable for this file.";
    $("drag-hint").style.visibility = "hidden";
    updateDragTarget();
  };
  candidate.src = url;
}

function updateDragTarget() {
  if (!bridge) return;
  const photo = selectedPhoto();
  const owner = photo && state.generation + "/" + photo.id + "/" + photo.format;
  if (!photo || state.busy || preview.hidden || $("review").hidden || shownOwner !== owner) {
    send("dragTarget").catch(() => {});
    return;
  }
  const { x, y, width, height } = preview.getBoundingClientRect();
  send("dragTarget", {
    generation: state.generation, id: photo.id, format: photo.format,
    rect: { x, y, width, height },
  }).catch(() => {});
}

function setImage(image, source) {
  if (image.getAttribute("src") === source) return;
  image.hidden = false;
  image.src = source;
}

function makeThumbnail(photo, grid) {
  const button = document.createElement("button");
  button.type = "button";
  button.className = grid ? "grid-card" : "tile";
  if (grid) {
    button.innerHTML = '<span class="grid-photo"><svg aria-hidden="true"><use href="#i-image"/></svg><img alt="" draggable="false"><svg class="grid-heart" aria-hidden="true"><use href="#i-heart"/></svg></span><span class="grid-caption"><span class="tile-name"></span><span class="grid-format"></span></span>';
  } else {
    button.innerHTML = '<img alt="" draggable="false"><span class="tile-name"></span><svg class="tile-heart" aria-hidden="true"><use href="#i-heart"/></svg>';
  }
  const image = button.querySelector("img");
  image.onerror = () => { image.hidden = true; };
  button.querySelector(".tile-name").textContent = photo.name;
  button.addEventListener("click", () => {
    if (state.editor || state.busy) return;
    if (grid) setView("viewer", photo.id);
    else select(photo.id);
  });
  return button;
}

function paintThumbnails(items, parent, first, last, isGrid, positionTile) {
  const present = new Set();
  for (let index = first; index < last; index++) {
    const photo = state.visible[index];
    present.add(photo.id);
    let tile = items.get(photo.id);
    if (!tile) {
      tile = makeThumbnail(photo, isGrid);
      items.set(photo.id, tile);
    }
    const current = photo.id === state.selected;
    const filename = photo[photo.format];
    tile.classList.toggle("current", current);
    tile.setAttribute("aria-label", filename + (photo.favorite ? ", favorite" : ""));
    tile.title = filename;
    if (current) tile.setAttribute("aria-current", "true");
    else tile.removeAttribute("aria-current");
    tile.querySelector(isGrid ? ".grid-heart" : ".tile-heart").style.display = photo.favorite ? "" : "none";
    if (isGrid) {
      const multiple = [photo.png, photo.raf, photo.hif, photo.jpg].filter(Boolean).length > 1;
      tile.querySelector(".grid-format").textContent = photo.format.toUpperCase() + (multiple ? " +" : "");
    }
    setImage(tile.querySelector("img"), previewURL(photo, "small"));
    positionTile(tile, index);
    const position = parent.children[index - first];
    if (tile !== position) parent.insertBefore(tile, position ?? null);
  }
  for (const [key, tile] of items) {
    if (present.has(key)) continue;
    tile.remove();
    items.delete(key);
  }
}

function revealTile(index) {
  const left = index * stride;
  const right = left + 104;
  if (left < scroll.scrollLeft) scroll.scrollLeft = left;
  else if (right > scroll.scrollLeft + scroll.clientWidth) scroll.scrollLeft = right - scroll.clientWidth;
}

function paintFilmstrip() {
  if ($("review").hidden) return;
  const first = Math.max(0, Math.floor(scroll.scrollLeft / stride) - 3);
  const last = Math.min(state.visible.length, Math.ceil((scroll.scrollLeft + scroll.clientWidth) / stride) + 3);
  paintThumbnails(tiles, track, first, last, false, (tile, index) => {
    tile.style.left = index * stride + "px";
  });
}

function gridGeometry() {
  const gap = 10;
  const width = Math.max(0, gridScroll.clientWidth - 2 * gap);
  const size = Math.min(Number(gridSize.value), width);
  const height = Math.round(size * 0.73) + 29;
  const columns = Math.max(1, Math.floor((width + gap) / (size + gap)));
  return { gap, size, height, columns, row: height + gap };
}

function paintGrid(reveal = null) {
  if ($("grid").hidden) return;
  const { gap, size, height, columns, row } = gridGeometry();
  gridTrack.style.height = (Math.ceil(state.visible.length / columns) * row + gap) + "px";
  if (resetScroll) {
    gridScroll.scrollTop = 0;
    resetScroll = false;
  }
  if (reveal !== null) {
    const top = Math.floor(reveal / columns) * row + gap;
    if (top < gridScroll.scrollTop) gridScroll.scrollTop = top - gap;
    else if (top + height > gridScroll.scrollTop + gridScroll.clientHeight) {
      gridScroll.scrollTop = top + height - gridScroll.clientHeight + gap;
    }
  }
  const first = Math.max(0, Math.floor(gridScroll.scrollTop / row) - 2) * columns;
  const last = Math.min(state.visible.length,
    (Math.ceil((gridScroll.scrollTop + gridScroll.clientHeight) / row) + 2) * columns);
  paintThumbnails(cards, gridTrack, first, last, true, (tile, index) => {
    tile.style.width = size + "px";
    tile.style.height = height + "px";
    tile.style.left = (gap + (index % columns) * (size + gap)) + "px";
    tile.style.top = (gap + Math.floor(index / columns) * row) + "px";
  });
}

function resetPreviews() {
  track.replaceChildren();
  gridTrack.replaceChildren();
  tiles.clear();
  cards.clear();
  resetScroll = true;
}

function scheduleLayout() {
  if (scheduledLayout) return;
  scheduledLayout = true;
  requestAnimationFrame(() => {
    scheduledLayout = false;
    paintFilmstrip();
    paintGrid();
    updateDragTarget();
  });
}

function select(id) {
  if (state.editor || state.busy) return;
  state.selected = id;
  paint({ reveal: true, recompute: false });
}

function step(direction) {
  const index = state.visible.findIndex(photo => photo.id === state.selected);
  const destination = state.visible[index + direction];
  if (destination) select(destination.id);
}

function filter(value) {
  if (!state.root || state.editor || state.busy || state.filter === value) return;
  state.filter = value;
  resetPreviews();
  paint({ reveal: true });
}

function setView(value, id = state.selected) {
  if (state.editor || state.busy) return;
  state.view = value;
  state.selected = id;
  saveSetting("framekeep-layout", value);
  resetScroll = true;
  paint({ reveal: true, recompute: false });
}

async function work(label, callback) {
  if (state.busy) return;
  state.busy = label;
  paint({ recompute: false });
  try {
    await callback();
  } catch (error) {
    notify(String(error?.message ?? error), { error: true });
  } finally {
    state.busy = "";
    paint({ reveal: true });
  }
}

function chooseFolder() {
  if (state.editor) return;
  work("Opening folder…", async () => {
    const result = await send("pick", state.root ? { root: state.root } : {});
    if (result) setFolder(result);
  });
}

function refresh() {
  if (!state.root || state.editor) return;
  work("Scanning folder…", async () => setFolder(await send("refresh"), true));
}

function photoCommand(photo) {
  return { generation: state.generation, id: photo.id, format: photo.format };
}

function changePhoto(op, params) {
  const photo = selectedPhoto();
  if (!photo || state.editor) return;
  const index = state.visible.findIndex(item => item.id === photo.id);
  const next = state.visible[index + 1];
  const previous = state.visible[index - 1];
  const labels = { review: "Saving decision…", favorite: "Saving favorite…", format: "Saving format…" };
  const creatingPng = op === "format" && params.format === "png" && !photo.png;
  work(creatingPng ? "Creating PNG companion…" : labels[op], async () => {
    const result = await send(op, { ...photoCommand(photo), ...params });
    state.photos[result.id] = result.photo;
    if (op === "review" || op === "favorite") state.canUndo = result.canUndo;
    if (creatingPng) notify("Created " + result.photo.png);
    if (!matches(result.photo, state.filter)) {
      state.selected = next?.id ?? previous?.id ?? null;
    }
  });
}

function review(value) {
  if (selectedPhoto()?.review !== value) changePhoto("review", { review: value });
}

function applyBatch(result) {
  for (const change of result.changes) state.photos[change.id] = change.photo;
  state.canUndo = result.canUndo;
  state.canUndoTrash = result.canUndoTrash;
}

function batchFeedback(result, successText) {
  const success = result.count ? countLabel(result.count, "file") + " " + successText : "";
  if (result.errorCount) {
    const more = result.errorCount - 1;
    const detail = result.errors[0] + (more ? " (+" + countLabel(more, "other issue") + ")" : "");
    notify([success, detail].filter(Boolean).join(" · "), { error: true, undo: result.canUndoTrash });
  } else if (success) notify(success, { undo: result.canUndoTrash });
}

function confirmRecycle() {
  if (state.editor || state.busy) return;
  const rawOnly = state.filter === "keep";
  const raws = rawOnly ? state.visible.filter(photo => photo.raf) : [];
  const count = rawOnly ? raws.length : state.visible.length;
  const pngsNeeded = raws.filter(photo => !photo.developed).length;
  if (!count) return;
  $("recycle-title").textContent = rawOnly ? "Trash all kept RAWs?" : "Recycle all photos?";
  $("recycle-confirm").textContent = rawOnly ? "Trash all RAWs" : "Recycle all";
  $("recycle-copy").textContent = rawOnly
    ? countLabel(count, "RAW file") + " will move to macOS Trash, including RAWs in Favorites. Existing developed versions stay in Keep." +
      (pngsNeeded ? " Framekeep will first create " + countLabel(pngsNeeded, "PNG") + " for RAWs without a developed version." : "")
    : countLabel(count, "photo") + " will move to macOS Trash, including every file version. You can undo this move.";
  $("confirm-recycle").showModal();
}

function paintRawTrashProgress(processed, total) {
  const meter = $("raw-trash-meter");
  meter.max = Math.max(1, total);
  meter.value = processed;
  $("raw-trash-count").textContent = processed.toLocaleString() + " of " + countLabel(total, "RAW") + " processed";
}

window.framekeepRawTrashProgress = (processed, total) => {
  if (!$("raw-trash-progress").hidden) paintRawTrashProgress(processed, total);
};

function recycleAll() {
  const rawOnly = state.filter === "keep";
  $("confirm-recycle").close();
  work(rawOnly ? "Moving kept RAWs to Trash…" : "Moving files to Trash…", async () => {
    if (rawOnly) {
      paintRawTrashProgress(0, state.visible.filter(photo => photo.raf).length);
      $("raw-trash-progress").hidden = false;
    }
    try {
      const result = await send(rawOnly ? "trashKeptRaws" : "recycleAll", { generation: state.generation });
      applyBatch(result);
      batchFeedback(result, "moved to Trash");
    } finally {
      $("raw-trash-progress").hidden = true;
    }
  });
}

function undo(trashOnly = false) {
  if (!(trashOnly ? state.canUndoTrash : state.canUndo) || state.editor) return;
  work(trashOnly ? "Restoring from Trash…" : "Undoing…", async () => {
    const result = await send(trashOnly ? "undoTrash" : "undo");
    if (result.review) {
      const { id, photo } = result.review;
      state.canUndo = result.canUndo;
      state.photos[id] = photo;
      if (photo) {
        if (!matches(photo, state.filter)) state.filter = photo.review;
        state.selected = id;
        resetPreviews();
      }
      notify("Review decision undone");
      return;
    }
    applyBatch(result);
    if (!state.canUndoTrash) $("toast-undo").hidden = true;
    if (result.count && result.changes[0]?.photo) {
      const restored = result.changes[0].photo;
      state.filter = restored.review;
      state.selected = result.changes[0].id;
      resetPreviews();
    }
    batchFeedback(result, "restored");
  });
}

function switchFormat(format) {
  const photo = selectedPhoto();
  if (photo && photo.format !== format && (photo[format] || (format === "png" && photo.raf && !photo.developed))) {
    changePhoto("format", { format });
  }
}

function startEdit() {
  const photo = selectedPhoto();
  if (!photo || state.busy || state.editor) return;
  const initial = editsFor(photo);
  state.editor = {
    edits: { ...initial }, initial, original: false,
    command: photoCommand(photo), persisted: JSON.stringify(initial), writes: Promise.resolve(),
  };
  paint({ recompute: false });
  (photo.format === "raf" ? $("grade") : $("exposure")).focus();
}

function paintEditor() {
  const { edits, original } = state.editor;
  const raw = selectedPhoto().format === "raf";
  const locked = Boolean(state.busy);
  $("edit-format").textContent = selectedPhoto().format.toUpperCase();
  $("grade-panel").hidden = !raw;
  const missingLut = edits.grade.startsWith("fuji_") && !lutGrades.has(edits.grade);
  $("missing-lut").hidden = !missingLut;
  $("missing-lut").value = missingLut ? edits.grade : "";
  $("missing-lut-note").hidden = !missingLut;
  $("grade").value = edits.grade;
  $("grade").disabled = locked;
  $("strength-field").hidden = edits.grade === "original";
  for (const input of editInputs) {
    const value = edits[input.id];
    input.value = value;
    input.disabled = locked;
    const label = input.id === "exposure" ? (value / 100).toFixed(2) + " EV"
      : input.id === "strength" ? value + "%" : String(value);
    document.querySelector('output[for="' + input.id + '"]').textContent = label;
  }
  $("compare").setAttribute("aria-pressed", String(original));
  for (const id of ["rotate", "compare", "reset-edit", "cancel-edit", "save-edit"]) $(id).disabled = locked;
  $("save-new-photo").disabled = locked || Object.keys(defaultEdits).every(key => edits[key] === defaultEdits[key]);
}

function persistEdits(editor, adjustments) {
  const edits = { ...adjustments };
  const key = JSON.stringify(edits);
  editor.writes = editor.writes.catch(() => {}).then(async () => {
    if (key === editor.persisted) return;
    const result = await send("adjust", { ...editor.command, edits });
    state.photos[result.id] = result.photo;
    editor.persisted = key;
  });
  return editor.writes;
}

function savePendingEdits() {
  clearTimeout(pendingEditsTimer);
  const editor = state.editor;
  if (!editor || state.busy) return;
  persistEdits(editor, editor.edits).catch(error => {
    if (state.editor === editor) notify(String(error?.message ?? error), { error: true });
  });
}

function changeDraft(callback, debounce = false) {
  if (!state.editor || state.busy) return;
  const previous = JSON.stringify(state.editor.edits);
  callback(state.editor);
  paintEditor();
  clearTimeout(draftTimer);
  if (debounce) draftTimer = setTimeout(() => showPreview(selectedPhoto()), 100);
  else showPreview(selectedPhoto());

  if (JSON.stringify(state.editor.edits) !== previous) {
    clearTimeout(pendingEditsTimer);
    if (debounce) pendingEditsTimer = setTimeout(savePendingEdits, 180);
    else savePendingEdits();
  }
}

function endEdit() {
  clearTimeout(draftTimer);
  clearTimeout(pendingEditsTimer);
  state.editor = null;
}

function closeEditor(cancel = false) {
  const editor = state.editor;
  if (!editor || state.busy) return;
  clearTimeout(pendingEditsTimer);
  work(cancel ? "Restoring previous adjustments…" : "Saving adjustments…", async () => {
    await persistEdits(editor, cancel ? editor.initial : editor.edits);
    endEdit();
  });
}

function saveNewPhoto() {
  const editor = state.editor;
  if (!editor || state.busy) return;
  clearTimeout(pendingEditsTimer);
  work("Saving new photo…", async () => {
    await persistEdits(editor, editor.edits);
    const result = await send("saveNewPhoto", editor.command);
    state.photos[result.source.id] = result.source.photo;
    state.photos[result.copy.id] = result.copy.photo;
    endEdit();
    notify("Saved " + result.filename);
  });
}

openButtons.forEach(button => button.addEventListener("click", chooseFolder));
filterButtons.forEach(button => button.addEventListener("click", () => filter(button.dataset.filter)));
$("view-raw").addEventListener("click", () => switchFormat("raf"));
$("view-developed").addEventListener("click", () => {
  const photo = selectedPhoto();
  if (photo) switchFormat(developedFor(photo));
});
$("developed-format").addEventListener("change", event => switchFormat(event.target.value));
viewButtons.forEach(button => button.addEventListener("click", () => setView(button.dataset.view)));
$("refresh").addEventListener("click", refresh);
$("previous").addEventListener("click", () => step(-1));
$("next").addEventListener("click", () => step(1));
$("trash").addEventListener("click", () => review("delete"));
$("keep").addEventListener("click", () => review("keep"));
$("favorite").addEventListener("click", () => changePhoto("favorite", { favorite: !selectedPhoto()?.favorite }));
$("unreview").addEventListener("click", () => review("unreviewed"));
$("recycle-all").addEventListener("click", confirmRecycle);
$("trash-kept-raws").addEventListener("click", confirmRecycle);
$("recycle-close").addEventListener("click", () => $("confirm-recycle").close());
$("recycle-cancel").addEventListener("click", () => $("confirm-recycle").close());
$("recycle-confirm").addEventListener("click", recycleAll);
$("undo").addEventListener("click", () => undo(true));
$("header-undo").addEventListener("click", () => undo(true));
$("toast-undo").addEventListener("click", () => undo(true));
$("toast-close").addEventListener("click", () => { $("toast").hidden = true; });
$("shortcuts").addEventListener("click", () => $("help").showModal());
$("help-close").addEventListener("click", () => $("help").close());
$("empty-action").addEventListener("click", () => {
  if (state.filter === "delete" && state.canUndoTrash) undo(true);
  else if (state.photos.some(Boolean)) filter("unreviewed");
  else chooseFolder();
});
$("edit").addEventListener("click", startEdit);
$("cancel-edit").addEventListener("click", () => closeEditor(true));
$("save-edit").addEventListener("click", () => closeEditor());
$("save-new-photo").addEventListener("click", saveNewPhoto);
$("rotate").addEventListener("click", () => changeDraft(editor => {
  editor.edits.rotation = (editor.edits.rotation + 1) % 4;
  editor.original = false;
}));
$("compare").addEventListener("click", () => changeDraft(editor => { editor.original = !editor.original; }));
$("reset-edit").addEventListener("click", () => changeDraft(editor => {
  editor.edits = { ...defaultEdits };
  editor.original = false;
}));
$("grade").addEventListener("change", event => changeDraft(editor => {
  editor.edits.grade = event.target.value;
  if (editor.edits.grade === "original") editor.edits.strength = 100;
  editor.original = false;
}));
for (const input of editInputs) {
  input.addEventListener("input", () => changeDraft(editor => {
    editor.edits[input.id] = Number(input.value);
    editor.original = false;
  }, true));
  input.addEventListener("change", savePendingEdits);
}

gridSize.addEventListener("input", () => {
  saveSetting("framekeep-grid-size", gridSize.value);
  paintGrid();
});
gridScroll.addEventListener("scroll", scheduleLayout, { passive: true });
scroll.addEventListener("scroll", scheduleLayout, { passive: true });
scroll.addEventListener("wheel", event => {
  if (Math.abs(event.deltaY) <= Math.abs(event.deltaX) || scroll.scrollWidth <= scroll.clientWidth) return;
  scroll.scrollLeft += event.deltaY;
  event.preventDefault();
}, { passive: false });
const resize = new ResizeObserver(scheduleLayout);
for (const element of [scroll, gridScroll, $("preview-area"), preview]) resize.observe(element);

window.framekeepMenu = op => {
  if (document.querySelector("dialog[open]")) return;
  if (op === "open") chooseFolder();
  else if (op === "refresh") refresh();
  else if (op === "undo") undo();
};

window.addEventListener("keydown", event => {
  if (document.querySelector("dialog[open]") || event.defaultPrevented || event.ctrlKey || event.altKey) return;
  const key = event.key.toLowerCase();
  if (event.metaKey) {
    if (key === "z" && !event.shiftKey && !state.editor) {
      event.preventDefault();
      if (!event.repeat) undo();
    }
    return;
  }
  if (state.editor) {
    if (key === "escape" && !state.busy) closeEditor(true);
    return;
  }
  if (event.target instanceof HTMLInputElement || event.target instanceof HTMLSelectElement) return;
  const photo = selectedPhoto();
  if (key === "?" && !event.repeat) $("help").showModal();
  else if (key === "g" && state.root && !event.repeat) setView(state.view === "grid" ? "viewer" : "grid");
  else if (state.view === "grid") return;
  else if (key === "arrowleft" || key === "arrowright") step(key === "arrowleft" ? -1 : 1);
  else if (event.repeat) return;
  else if (photo && key === "k") review("keep");
  else if (photo && key === "h") changePhoto("favorite", { favorite: !photo.favorite });
  else if (photo && key === "v") switchFormat(photo.format === "raf" ? developedFor(photo) : "raf");
  else if (photo && (key === "backspace" || key === "delete")) review("delete");
  else return;
  event.preventDefault();
});

if (bridge) {
  work("Scanning folder…", async () => setFolder(await send("startup")));
  send("lutOptions").then(options => {
    const group = $("fujifilm-grades");
    for (const { grade, label } of options) {
      lutGrades.add(grade);
      group.append(new Option(label, grade));
    }
    group.hidden = options.length === 0;
    if (state.editor) paintEditor();
  }).catch(error => notify(String(error?.message ?? error), { error: true }));
} else paint();
