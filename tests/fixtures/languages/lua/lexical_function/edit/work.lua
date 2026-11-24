local function emit() end
local function flush() end
local function run()
  flush()
end
do
  local function emit() end
end
