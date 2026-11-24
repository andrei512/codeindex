cmake_minimum_required(VERSION 3.20)
project(Fixture NONE)
function(ping)
endfunction()
#[=[
function(ghost)
]=]
set(text [=[ping()]=])
PING()
