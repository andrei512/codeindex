@echo off
:entry
call :newTask
goto :render
rem call :render
:render
exit /b
:newTask
exit /b
