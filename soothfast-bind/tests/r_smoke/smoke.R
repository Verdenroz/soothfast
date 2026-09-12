args <- commandArgs(trailingOnly = TRUE)
.libPaths(c(args[1], .libPaths()))
library(acme.core)

x <- Counter(10)
cat("value=", x$value(), "\n", sep = "")
cat("bump=", x$bump(5), "\n", sep = "")
cat("at(Low)=", x$at("Low"), "\n", sep = "")
cat("at(High)=", x$at("High"), "\n", sep = "")
cat("bump_all=", x$bump_all(c(1, 2, 3)), "\n", sep = "")

cat("digest=", paste(digest(as.raw(c(1, 2, 3))), collapse = " "), "\n", sep = "")
cat("normalize=", paste(normalize(c(1, 2, 3), 2), collapse = " "), "\n", sep = "")
cat("greet=", greet("R"), "\n", sep = "")
cat("stamp=", stamp(1, 2, as.raw(c(1, 2, 3))), "\n", sep = "")

cat("trim(empty)=", is.null(trim(numeric(0))), "\n", sep = "")
cat("trim=", paste(trim(c(1, 2)), collapse = " "), "\n", sep = "")

err <- tryCatch(x$at("Bogus"), error = function(e) conditionMessage(e))
cat("caught:", err, "\n")
