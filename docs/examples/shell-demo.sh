#!/bin/sh
# ElaraOS script-language subset; no external host programs.
title="星澜脚本"
echo "$title"

greet() {
    printf 'hello %s\n' "$1"
}

for name in Alice Bob; do
    greet "$name"
done

if test "$#" -eq 0; then
    echo "no args"
else
    printf 'arg=%s\n' "$1"
fi

printf '%s\n' red green blue | wc -l
