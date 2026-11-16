import { pageTitle, type RouteHandle } from '@/lib/route-title';
import { DeviceAuthPage } from '@/pages/DeviceAuthPage';

export const handle: RouteHandle = pageTitle('cliAuth.title');

export default function DeviceAuth() {
  return <DeviceAuthPage />;
}
