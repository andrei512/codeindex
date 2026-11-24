<?php
$doc = <<<'TEXT'
function phantom() { actual(); }
TEXT;
function actual() {}
actual();
