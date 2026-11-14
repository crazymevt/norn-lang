// The C side of ffi_test.norn (M9 part 9.8b): structs passed and returned
// by value, unions, bit fields, variadic functions, and callbacks taking
// and returning structs, called from Norn and from a thread of C's own.
// ffi_test.norn builds it with cc into a shared library.

#include <pthread.h>
#include <stdarg.h>
#include <stddef.h>
#include <stdint.h>
#include <string.h>

typedef struct { int a; int b; } Pair;           // 8 bytes: one register
typedef struct { int a; char b; int c; } Odd12;  // 12 bytes: two registers
typedef struct { float x, y; } V2;               // 2 floats: float registers
typedef struct { double x, y, z, w; } V4;        // 4 doubles: float registers
typedef struct { long a, b, c; } Big;            // 24 bytes: by reference
typedef struct { char c; double d; short s; } Padded;
typedef struct {
    unsigned a : 3;
    unsigned b : 5;
    int c : 4;
    unsigned d : 22;
    unsigned char e : 2;
    unsigned long long f : 40;
} Flags;
typedef union { int i; float f; char c[6]; double d; } Any;
typedef struct { char tag; Any u; } Tagged;

// ---- layouts, as C sees them ----

long layout(int i) {
    switch (i) {
    case 0: return sizeof(Odd12);
    case 1: return offsetof(Odd12, c);
    case 2: return sizeof(Padded);
    case 3: return offsetof(Padded, d);
    case 4: return offsetof(Padded, s);
    case 5: return sizeof(Flags);
    case 6: return _Alignof(Flags);
    case 7: return sizeof(Any);
    case 8: return _Alignof(Any);
    case 9: return sizeof(Tagged);
    case 10: return offsetof(Tagged, u);
    default: return -1;
    }
}

// ---- bit fields and unions ----

void flags_set(Flags *f, unsigned a, unsigned b, int c, unsigned d, unsigned e, unsigned long long g) {
    f->a = a; f->b = b; f->c = c; f->d = d; f->e = e; f->f = g;
}

long long flags_get(Flags *f, int which) {
    switch (which) {
    case 0: return f->a;
    case 1: return f->b;
    case 2: return f->c;
    case 3: return f->d;
    case 4: return f->e;
    default: return (long long)f->f;
    }
}

float any_float(Any *u) { return u->f; }
void any_set_int(Any *u, int i) { u->i = i; }
double any_double(Any *u) { return u->d; }

// ---- structs by value ----

Pair pair_make(int a, int b) { Pair p = {a, b}; return p; }
int pair_sum(Pair p) { return p.a + p.b; }
Odd12 odd_make(int a, char b, int c) { Odd12 o = {a, b, c}; return o; }
long odd_sum(Odd12 o) { return o.a + o.b * 10 + o.c * 100; }
V2 v2_add(V2 a, V2 b) { V2 r = {a.x + b.x, a.y + b.y}; return r; }
V4 v4_scale(V4 v, double k) { V4 r = {v.x * k, v.y * k, v.z * k, v.w * k}; return r; }
double v4_sum(V4 v) { return v.x + v.y + v.z + v.w; }
Big big_make(long a) { Big b = {a, a * 2, a * 3}; return b; }
long big_sum(Big b) { return b.a + b.b + b.c; }
Padded padded_make(char c, double d, short s) { Padded p = {c, d, s}; return p; }
double padded_sum(Padded p) { return p.c + p.d + p.s; }
long tagged_check(Tagged t) { return t.tag * 1000 + t.u.i; }
long flags_sum(Flags f) { return f.a + f.b + f.c + f.d + f.e + (long)f.f; }

// (the registers run out: the struct goes on the stack, whole)
long many_ints(long a1, long a2, long a3, long a4, long a5, long a6, long a7, Odd12 o, long after) {
    return a1 + a2 + a3 + a4 + a5 + a6 + a7 + odd_sum(o) * 1000 + after * 1000000;
}
double many_floats(double d1, double d2, double d3, double d4, double d5, double d6, double d7, V2 v, double after) {
    return d1 + d2 + d3 + d4 + d5 + d6 + d7 + (v.x + v.y) * 100 + after * 10000;
}

// ---- callbacks with structs ----

Pair apply_pair(Pair (*f)(Pair, int), Pair p, int k) { return f(p, k); }
V4 apply_v4(V4 (*f)(V4, double), V4 v) { return f(v, 2.0); }
Big apply_big(Big (*f)(Big), Big b) { return f(b); }
long apply_many(long (*f)(long, long, long, long, long, long, long, Odd12, long)) {
    Odd12 o = {1, 2, 3};
    return f(1, 2, 3, 4, 5, 6, 7, o, 8);
}
double apply_many_floats(double (*f)(double, double, double, double, double, double, double, V2, double)) {
    V2 v = {0.5f, 0.25f};
    return f(1, 2, 3, 4, 5, 6, 7, v, 8);
}

// ---- variadic ----

long sum_longs(int n, ...) {
    va_list ap;
    va_start(ap, n);
    long s = 0;
    for (int i = 0; i < n; i++) s += va_arg(ap, long);
    va_end(ap);
    return s;
}

double sum_doubles(int n, ...) {
    va_list ap;
    va_start(ap, n);
    double s = 0;
    for (int i = 0; i < n; i++) s += va_arg(ap, double);
    va_end(ap);
    return s;
}

// kinds: i int, l long, d double, s string (its length)
double mixed(const char *kinds, ...) {
    va_list ap;
    va_start(ap, kinds);
    double s = 0;
    for (const char *k = kinds; *k; k++) {
        switch (*k) {
        case 'i': s += va_arg(ap, int); break;
        case 'l': s += va_arg(ap, long); break;
        case 'd': s += va_arg(ap, double); break;
        case 's': s += strlen(va_arg(ap, const char *)); break;
        }
    }
    va_end(ap);
    return s;
}

// ---- a thread of C's own ----

struct job { int (*f)(int); int x; int r; };

static void *run_job(void *p) {
    struct job *j = p;
    j->r = j->f(j->x);
    return NULL;
}

int call_from_thread(int (*f)(int), int x) {
    struct job j = {f, x, 0};
    pthread_t t;
    pthread_create(&t, NULL, run_job, &j);
    pthread_join(t, NULL);
    return j.r;
}
