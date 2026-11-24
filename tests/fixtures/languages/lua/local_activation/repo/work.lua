local function emit() return 7 end
do local emit = emit(); assert(emit == 7) end
do local first, emit = 1, function() return 2 end; assert(emit() == 2) end
for _, emit in ipairs({function() return 3 end}) do assert(emit() == 3) end
repeat local function emit() return true end until emit()
assert(emit() == 7)
