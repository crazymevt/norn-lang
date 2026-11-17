;;;; bench/programs.skn's Mandelbrot in SBCL, for comparison: the same view,
;;;; the same escape-time kernel with smooth colouring (mandelbrot.fractal's
;;;; escape), typed double-floats and (speed 3), as an SBCL program would be
;;;; written for speed. Prints the best of the runs in about 2 s, in ms,
;;;; and the checksum, which must match the Skein program's.
;;;;
;;;;   sbcl --script bench/mandelbrot.lisp

(declaim (optimize (speed 3) (safety 0) (debug 0)))

(declaim (inline escape))
(defun escape (re im limit)
  (declare (type double-float re im) (type fixnum limit))
  (let* ((x (- re 0.25d0))
         (im2 (* im im))
         (q (+ (* x x) im2))
         (bulb (+ re 1d0)))
    (if (or (< (* q (+ q x)) (* 0.25d0 im2))
            (< (+ (* bulb bulb) im2) 0.0625d0))
        -1d0
        (let ((zr 0d0) (zi 0d0) (n 0))
          (declare (type double-float zr zi) (type fixnum n))
          (loop
            (let* ((zr2 (* zr zr))
                   (zi2 (* zi zi))
                   (radius2 (+ zr2 zi2)))
              (cond ((> radius2 16d0)
                     (return (- (+ n 1d0) (log (* 0.5d0 (log radius2 2d0)) 2d0))))
                    ((= n limit) (return -1d0))
                    (t (let ((tt (* zr zi)))
                         (setf zr (+ (- zr2 zi2) re)
                               zi (+ tt tt im)
                               n (1+ n)))))))))))

(defun mandelbrot ()
  (let* ((re -0.7463d0) (im 0.1102d0) (span 0.028d0) (width 200) (height 125) (limit 220)
         (scale (/ span (min width height)))
         (left (- re (* width 0.5d0 scale)))
         (top (+ im (* height 0.5d0 scale)))
         (inside 0) (total 0))
    (declare (type double-float re im span scale left top) (type fixnum width height limit inside total))
    (dotimes (y height)
      (let ((ci (- top (* (+ y 0.5d0) scale))))
        (dotimes (x width)
          (let ((n (escape (+ left (* (+ x 0.5d0) scale)) ci limit)))
            (if (minusp n) (incf inside) (incf total (floor n)))))))
    (list inside total)))

(let ((best most-positive-fixnum) (result nil)
      (end (+ (get-internal-real-time) (* 2 internal-time-units-per-second))))
  (mandelbrot)
  (loop while (< (get-internal-real-time) end)
        do (let ((t0 (get-internal-real-time)))
             (setf result (mandelbrot))
             (setf best (min best (- (get-internal-real-time) t0)))))
  (format t "~,3f ~a ~a~%" (/ (* best 1000d0) internal-time-units-per-second) (first result) (second result)))
