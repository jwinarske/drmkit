// T7 property-write trace shim.
//
// Captures every atomic modeset ioctl and writes a normalized, textual trace
// of the (object, property, value) writes it carries.
//
// The capture point is the ioctl boundary rather than either implementation's
// own request builder. That is deliberate: it means neither drm-cxx nor drmkit
// has to be instrumented to be traced, so the C++ tree stays pristine as the
// origin of authority, and the two traces are produced by the *same* code
// reading the *same* struct. A differ over two traces taken at different
// levels would be comparing the harness against itself as much as the
// implementations.
//
// Only works for callers that route ioctls through libc. See parity/README.md.
//
//   cc -shared -fPIC -O2 -o trace-shim.so trace-shim.c -ldl
//   DRMKIT_TRACE=out.trace LD_PRELOAD=./trace-shim.so ./some-test

#define _GNU_SOURCE
#include <dlfcn.h>
#include <stdarg.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <drm/drm.h>
#include <drm/drm_mode.h>

static int (*real_ioctl)(int, unsigned long, ...);
static FILE *trace;
static int resolved_fd = -1;

// --- object-id normalization -------------------------------------------------
//
// Raw KMS object ids are assigned by the kernel and differ between runs and
// between the two implementations' probe order. A trace that quoted them would
// diff dirty for reasons that have nothing to do with behavior, so every id is
// rewritten to a stable role: crtc[0], plane[3], connector[1].

#define MAX_OBJ 128
static struct { uint32_t id; char name[24]; } objs[MAX_OBJ];
static int n_objs;

#define MAX_PROP 256
static struct { uint32_t id; char name[DRM_PROP_NAME_LEN]; } props[MAX_PROP];
static int n_props;

// Framebuffer and blob ids are allocation-dependent in the same way, but unlike
// KMS objects they are created during the run, so they cannot be enumerated up
// front. They are numbered in order of first appearance instead: what a trace
// asserts is that the same *distinct* buffer comes back on the same frame, not
// which handle the kernel happened to hand out.
#define MAX_ALLOC 512
static uint32_t alloc_ids[MAX_ALLOC];
static int n_allocs;

static int alloc_index(uint32_t id) {
  for (int i = 0; i < n_allocs; i++)
    if (alloc_ids[i] == id) return i;
  if (n_allocs < MAX_ALLOC) { alloc_ids[n_allocs] = id; return n_allocs++; }
  return -1;
}

static void record(uint32_t id, const char *kind, int index) {
  if (n_objs >= MAX_OBJ) return;
  objs[n_objs].id = id;
  snprintf(objs[n_objs].name, sizeof objs[n_objs].name, "%s[%d]", kind, index);
  n_objs++;
}

static const char *obj_name(uint32_t id) {
  for (int i = 0; i < n_objs; i++)
    if (objs[i].id == id) return objs[i].name;
  return "unknown";
}

static const char *prop_name(int fd, uint32_t id) {
  for (int i = 0; i < n_props; i++)
    if (props[i].id == id) return props[i].name;
  struct drm_mode_get_property gp;
  memset(&gp, 0, sizeof gp);
  gp.prop_id = id;
  if (real_ioctl(fd, DRM_IOCTL_MODE_GETPROPERTY, &gp) != 0) return "unknown";
  if (n_props >= MAX_PROP) return "unknown";
  props[n_props].id = id;
  memcpy(props[n_props].name, gp.name, DRM_PROP_NAME_LEN);
  props[n_props].name[DRM_PROP_NAME_LEN - 1] = 0;
  return props[n_props++].name;
}

static void resolve_objects(int fd) {
  if (resolved_fd == fd) return;
  resolved_fd = fd;
  n_objs = 0;

  struct drm_mode_card_res res;
  memset(&res, 0, sizeof res);
  if (real_ioctl(fd, DRM_IOCTL_MODE_GETRESOURCES, &res) != 0) return;

  uint32_t *crtcs = calloc(res.count_crtcs ? res.count_crtcs : 1, 4);
  uint32_t *conns = calloc(res.count_connectors ? res.count_connectors : 1, 4);
  if (!crtcs || !conns) { free(crtcs); free(conns); return; }
  res.crtc_id_ptr = (uint64_t)(uintptr_t)crtcs;
  res.connector_id_ptr = (uint64_t)(uintptr_t)conns;
  res.fb_id_ptr = 0; res.count_fbs = 0;
  res.encoder_id_ptr = 0; res.count_encoders = 0;
  if (real_ioctl(fd, DRM_IOCTL_MODE_GETRESOURCES, &res) == 0) {
    for (uint32_t i = 0; i < res.count_crtcs; i++) record(crtcs[i], "crtc", i);
    for (uint32_t i = 0; i < res.count_connectors; i++) record(conns[i], "connector", i);
  }
  free(crtcs); free(conns);

  struct drm_mode_get_plane_res pres;
  memset(&pres, 0, sizeof pres);
  if (real_ioctl(fd, DRM_IOCTL_MODE_GETPLANERESOURCES, &pres) != 0) return;
  uint32_t *planes = calloc(pres.count_planes ? pres.count_planes : 1, 4);
  if (!planes) return;
  pres.plane_id_ptr = (uint64_t)(uintptr_t)planes;
  if (real_ioctl(fd, DRM_IOCTL_MODE_GETPLANERESOURCES, &pres) == 0)
    for (uint32_t i = 0; i < pres.count_planes; i++) record(planes[i], "plane", i);
  free(planes);
}

// --- value rendering ---------------------------------------------------------

static int is_alloc_prop(const char *name) {
  return strcmp(name, "FB_ID") == 0 || strcmp(name, "MODE_ID") == 0 ||
         strcmp(name, "CRTC_ID") == 0 || strcmp(name, "IN_FORMATS") == 0;
}

static void render_value(char *buf, size_t n, const char *pname, uint64_t v) {
  // A zero FB_ID or CRTC_ID is not an allocation, it is the disable sentinel,
  // and it has to render identically on both sides or every disable diffs.
  if (v == 0 && is_alloc_prop(pname)) { snprintf(buf, n, "0"); return; }
  if (strcmp(pname, "CRTC_ID") == 0) { snprintf(buf, n, "%s", obj_name((uint32_t)v)); return; }
  if (is_alloc_prop(pname)) {
    int idx = alloc_index((uint32_t)v);
    snprintf(buf, n, "#%d", idx);
    return;
  }
  // An acquire fence is passed as a descriptor number, which depends on how
  // many files the process happened to have open -- so the two runners write
  // different integers for identical behaviour. What has to match is whether a
  // fence was armed at all, and -1 is the kernel's "none" rather than an fd.
  if (strcmp(pname, "IN_FENCE_FD") == 0) {
    if ((int64_t)v < 0) { snprintf(buf, n, "none"); return; }
    snprintf(buf, n, "fence");
    return;
  }
  // Likewise a userspace address, which never matches across processes.
  if (strcmp(pname, "OUT_FENCE_PTR") == 0) {
    snprintf(buf, n, v ? "ptr" : "0");
    return;
  }
  snprintf(buf, n, "%llu", (unsigned long long)v);
}

// --- interception ------------------------------------------------------------

static void emit(int fd, struct drm_mode_atomic *a) {
  resolve_objects(fd);

  uint32_t *obj_ids = (uint32_t *)(uintptr_t)a->objs_ptr;
  uint32_t *nprops = (uint32_t *)(uintptr_t)a->count_props_ptr;
  uint32_t *prop_ids = (uint32_t *)(uintptr_t)a->props_ptr;
  uint64_t *vals = (uint64_t *)(uintptr_t)a->prop_values_ptr;
  if (!obj_ids || !nprops || !prop_ids || !vals) return;

  int test = (a->flags & DRM_MODE_ATOMIC_TEST_ONLY) != 0;
  int nonblock = (a->flags & DRM_MODE_ATOMIC_NONBLOCK) != 0;
  int evt = (a->flags & DRM_MODE_PAGE_FLIP_EVENT) != 0;
  fprintf(trace, "commit kind=%s%s%s\n", test ? "test" : "apply",
          nonblock ? " nonblock" : "", evt ? " event" : "");

  size_t cursor = 0;
  for (uint32_t i = 0; i < a->count_objs; i++) {
    for (uint32_t j = 0; j < nprops[i]; j++, cursor++) {
      const char *pn = prop_name(fd, prop_ids[cursor]);
      char rendered[64];
      render_value(rendered, sizeof rendered, pn, vals[cursor]);
      fprintf(trace, "  %s.%s = %s\n", obj_name(obj_ids[i]), pn, rendered);
    }
  }
  fflush(trace);
}

int ioctl(int fd, unsigned long request, ...) {
  va_list ap;
  va_start(ap, request);
  void *arg = va_arg(ap, void *);
  va_end(ap);

  if (!real_ioctl) real_ioctl = dlsym(RTLD_NEXT, "ioctl");

  if (request == DRM_IOCTL_MODE_ATOMIC && arg) {
    if (!trace) {
      const char *path = getenv("DRMKIT_TRACE");
      trace = path ? fopen(path, "w") : NULL;
      if (!trace) trace = stderr;
    }
    // Trace the request as submitted, then report what the kernel said: a
    // rejected commit is a behavior the two implementations must agree on, and
    // it is invisible in the writes alone.
    int rc = real_ioctl(fd, request, arg);
    emit(fd, (struct drm_mode_atomic *)arg);
    fprintf(trace, "  -> %s\n", rc == 0 ? "ok" : "rejected");
    fflush(trace);
    return rc;
  }
  return real_ioctl(fd, request, arg);
}
