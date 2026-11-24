local function emit() end
local function flush() end
local function run()
  emit()
end
do
  local function emit() end
end
