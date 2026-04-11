APP_NAME = Vibe Coding Rings
BINARY = vibe-coding-rings
VERSION = 1.1.0

.PHONY: build release app clean

build:
	cargo build --release

release: build app

app: build
	@echo "Creating .app bundle..."
	@rm -rf "dist/$(APP_NAME).app"
	@mkdir -p "dist/$(APP_NAME).app/Contents/MacOS"
	@mkdir -p "dist/$(APP_NAME).app/Contents/Resources/static"
	cp "target/release/$(BINARY)" "dist/$(APP_NAME).app/Contents/MacOS/$(BINARY)"
	cp Info.plist "dist/$(APP_NAME).app/Contents/Info.plist"
	cp -r static/* "dist/$(APP_NAME).app/Contents/Resources/static/"
	@echo "Done: dist/$(APP_NAME).app"

dmg: app
	@echo "Creating DMG..."
	@hdiutil create -volname "$(APP_NAME)" -srcfolder "dist/$(APP_NAME).app" -ov -format UDZO "dist/$(APP_NAME)-$(VERSION).dmg"
	@echo "Done: dist/$(APP_NAME)-$(VERSION).dmg"

clean:
	cargo clean
	rm -rf dist
