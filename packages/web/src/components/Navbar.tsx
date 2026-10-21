import { useAuth } from '@broccoli/web-sdk/auth';
import { useTranslation } from '@broccoli/web-sdk/i18n';
import { Slot } from '@broccoli/web-sdk/slot';
import {
  Button,
  Sheet,
  SheetContent,
  SheetTrigger,
} from '@broccoli/web-sdk/ui';
import { Menu } from 'lucide-react';
import { Link, useNavigate, useParams } from 'react-router';

import {
  type DashboardTab,
  useContest,
} from '@/features/contest/contexts/contest-context';

const defaultNavLinks = [
  { textKey: 'nav.contestInfo', href: '#' },
  { textKey: 'nav.problems', href: '/problems' },
  { textKey: 'nav.submissions', href: '#' },
  { textKey: 'nav.ranking', href: '#' },
];

const contestTabs: { textKey: string; tab: DashboardTab }[] = [
  { textKey: 'nav.problems', tab: 'problems' },
  { textKey: 'nav.submissions', tab: 'submissions' },
  { textKey: 'nav.ranking', tab: 'ranking' },
];

export function Navbar() {
  const { t } = useTranslation();
  const { user, logout } = useAuth();
  const { contestId, contestTitle, activeTab, setActiveTab, viewSubmissions } =
    useContest();
  const navigate = useNavigate();
  const params = useParams();

  const handleTabClick = (tab: DashboardTab) => {
    if (tab === 'submissions') {
      const problemId = params.problemId ? Number(params.problemId) : undefined;
      viewSubmissions(problemId);
    } else {
      setActiveTab(tab);
    }
    navigate('/');
  };

  return (
    <header className="sticky top-8 z-50 -mb-4 px-4 pb-4 -translate-y-8">
      <div className="fade-bottom bg-background/15 absolute left-0 h-16 w-full backdrop-blur-lg" />
      <div className="max-w-container relative mx-auto">
        <div className="flex h-16 items-center justify-between">
          <div className="flex items-center gap-6">
            <Slot name="navbar.brand" as="div" />
            <nav className="hidden md:flex items-center">
              {contestId ? (
                <>
                  {contestTitle && (
                    <span className="px-3 py-2 text-sm font-semibold">
                      {contestTitle}
                    </span>
                  )}
                  {contestTabs.map((item) => (
                    <button
                      key={item.tab}
                      onClick={() => handleTabClick(item.tab)}
                      className={`px-3 py-2 text-sm transition-colors cursor-pointer ${
                        activeTab === item.tab
                          ? 'text-primary font-medium'
                          : 'text-muted-foreground hover:text-primary'
                      }`}
                    >
                      {t(item.textKey)}
                    </button>
                  ))}
                </>
              ) : (
                defaultNavLinks.map((link) => (
                  <Link
                    key={link.textKey}
                    to={link.href}
                    className="px-3 py-2 text-sm hover:text-primary"
                  >
                    {t(link.textKey)}
                  </Link>
                ))
              )}
              <Slot name="navbar.menu" as="div" />
            </nav>
          </div>
          <div className="flex items-center gap-4">
            {user ? (
              <>
                <span className="hidden text-sm md:block">
                  {user.display_name}
                </span>
                <Button variant="outline" onClick={logout}>
                  <Link to="/">{t('auth.logout')}</Link>
                </Button>
              </>
            ) : (
              <>
                <Link to="/login" className="hidden text-sm md:block">
                  {t('nav.signIn')}
                </Link>
                <Button variant="default" asChild>
                  <Link to="/register">{t('nav.signUp')}</Link>
                </Button>
              </>
            )}
            <Slot name="navbar.actions" as="div" />
            <Sheet>
              <SheetTrigger asChild>
                <Button
                  variant="ghost"
                  size="icon"
                  className="shrink-0 md:hidden"
                >
                  <Menu className="size-5" />
                  <span className="sr-only">{t('nav.toggleMenu')}</span>
                </Button>
              </SheetTrigger>
              <SheetContent side="right">
                <nav className="grid gap-6 text-lg font-medium">
                  <Link
                    to="/"
                    className="flex items-center gap-2 text-xl font-bold"
                  >
                    <span>{t('app.name')}</span>
                  </Link>
                  {contestId
                    ? contestTabs.map((item) => (
                        <button
                          key={item.tab}
                          onClick={() => handleTabClick(item.tab)}
                          className={`text-left cursor-pointer ${
                            activeTab === item.tab
                              ? 'text-foreground font-medium'
                              : 'text-muted-foreground hover:text-foreground'
                          }`}
                        >
                          {t(item.textKey)}
                        </button>
                      ))
                    : defaultNavLinks.map((link) => (
                        <Link
                          key={link.textKey}
                          to={link.href}
                          className="text-muted-foreground hover:text-foreground"
                        >
                          {t(link.textKey)}
                        </Link>
                      ))}
                  <Slot name="navbar.mobile.menu" as="div" />
                </nav>
              </SheetContent>
            </Sheet>
          </div>
        </div>
      </div>
    </header>
  );
}
