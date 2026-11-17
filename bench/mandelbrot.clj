;;;; bench/programs.skn's Mandelbrot in Clojure, for comparison, as
;;;; bench/mandelbrot.lisp is in SBCL: the same view, the same escape-time
;;;; kernel with smooth colouring (mandelbrot.fractal's escape), with
;;;; primitive doubles and longs (type hints, unchecked arithmetic), as a
;;;; Clojure program would be written for speed. The JVM compiles hot code
;;;; as it runs, so it warms up for about 1 s first; then it prints the best
;;;; of the runs in about 2 s, in ms, and the checksum, which must match the
;;;; Skein program's.
;;;;
;;;;   clojure -M bench/mandelbrot.clj

(set! *warn-on-reflection* true)
(set! *unchecked-math* :warn-on-boxed)

(def ^:const ln2 (Math/log 2.0))

(defn- log2 ^double [^double x] (/ (Math/log x) ln2))

(defn escape
  "Smoothed escape count for c, or -1.0 when c stays within the bailout."
  ^double [^double re ^double im ^long limit]
  (let [x (- re 0.25)
        im2 (* im im)
        q (+ (* x x) im2)
        bulb (+ re 1.0)]
    (if (or (< (* q (+ q x)) (* 0.25 im2))
            (< (+ (* bulb bulb) im2) 0.0625))
      -1.0
      (loop [zr 0.0 zi 0.0 n 0]
        (let [zr2 (* zr zr)
              zi2 (* zi zi)
              radius2 (+ zr2 zi2)]
          (cond
            (> radius2 16.0) (- (+ n 1.0) (log2 (* 0.5 (log2 radius2))))
            (== n limit) -1.0
            :else (let [t (* zr zi)]
                    (recur (+ (- zr2 zi2) re) (+ t t im) (inc n)))))))))

(defn mandelbrot
  "Every pixel of the view: [interior pixels, the sum of the escape counts'
  whole parts]."
  []
  (let [re -0.7463 im 0.1102 span 0.028 width 200 height 125 limit 220
        scale (/ span (double (min width height)))
        left (- re (* width 0.5 scale))
        top (+ im (* height 0.5 scale))]
    (loop [y 0 inside 0 total 0]
      (if (== y height)
        [inside total]
        (let [ci (- top (* (+ y 0.5) scale))
              [i t] (loop [x 0 i 0 t 0]
                      (if (== x width)
                        [i t]
                        (let [n (escape (+ left (* (+ x 0.5) scale)) ci limit)]
                          (if (neg? n) (recur (inc x) (inc i) t) (recur (inc x) i (+ t (long n)))))))]
          (recur (inc y) (+ inside (long i)) (+ total (long t))))))))

(defn- run-for
  "Run mandelbrot for about ms milliseconds: [the best run in ns, its result]."
  [^long ms]
  (let [end (+ (System/nanoTime) (* ms 1000000))]
    (loop [best Long/MAX_VALUE result nil]
      (if (< (System/nanoTime) end)
        (let [t0 (System/nanoTime)
              r (mandelbrot)]
          (recur (min best (- (System/nanoTime) t0)) r))
        [best result]))))

(run-for 1000)
(let [[best [inside total]] (run-for 2000)]
  (println (format "%.3f %d %d" (/ (double best) 1e6) inside total)))
