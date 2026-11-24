FROM scratch AS build
COPY artifact /app
FROM scratch AS deliver
COPY --from=build /app /bin/app
LABEL note="build"
