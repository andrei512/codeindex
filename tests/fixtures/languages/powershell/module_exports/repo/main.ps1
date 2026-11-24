Import-Module ./Helpers.psm1
Helpers\Public
Helpers\Hidden
Helpers\Nested
$text = @'
The literal contains a ' quote.
function Phantom {}
'@
function Visible {}
