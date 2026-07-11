import { QueueListIcon } from "@heroicons/react/24/outline";
import { Button, Drawer, useOverlayState } from "@heroui/react";
import type { JSX } from "react";

import { Channels } from "./Channels";

export function MobileChannels(): JSX.Element {
  const drawerState = useOverlayState();

  return (
    <Drawer state={drawerState}>
      <Button aria-label="Open channel" className="md:hidden" isIconOnly variant="ghost">
        <QueueListIcon />
      </Button>
      <Drawer.Backdrop variant="blur">
        <Drawer.Content placement="left">
          <Drawer.Dialog>
            <Drawer.CloseTrigger aria-label="Close" />
            <Drawer.Header>
              <Drawer.Heading>Channels</Drawer.Heading>
            </Drawer.Header>
            <Drawer.Body className="mt-3">
              <Channels onServiceChange={drawerState.close} />
            </Drawer.Body>
          </Drawer.Dialog>
        </Drawer.Content>
      </Drawer.Backdrop>
    </Drawer>
  );
}
